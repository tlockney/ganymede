//! An app running the server with kernels of its own, as the iPadOS shell
//! will: no kernelspec dirs, no subprocess, no ZeroMQ. Passes with
//! `--no-default-features`.

use async_trait::async_trait;
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    in_process, in_process_pair, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner, TransportFactory,
};
use ganymede_server::config::Config;
use ganymede_server::serve::{run_with, Startup};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct App;

impl KernelspecSource for App {
    fn list(&self) -> ganymede_kernel_host::Result<Vec<Kernelspec>> {
        let mut spec: Kernelspec = serde_json::from_value(json!({
            "argv": [],
            "display_name": "In-app",
            "language": "fake",
        }))?;
        spec.name = "local".into();
        Ok(vec![spec])
    }
}

#[async_trait]
impl Provisioner for App {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(in_process())
    }
}

#[async_trait]
impl TransportFactory for App {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

/// One request with the token; the status code and JSON body.
async fn call(addr: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: token t\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap();
    let code = head[9..12].parse().unwrap();
    (code, serde_json::from_str(rest).unwrap_or(Value::Null))
}

#[tokio::test]
async fn serves_the_apps_kernels() {
    let root = tempfile::tempdir().unwrap();
    let cfg = Config {
        port: 0,
        token: Some("t".into()),
        root_dir: root.path().into(),
        cookie_secret_file: Some(root.path().join("secret")),
        notary_dir: Some(root.path().join("notary")),
        ..Default::default()
    };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    let app = Arc::new(App);
    let server = tokio::spawn(run_with(
        cfg,
        Startup {
            on_listening: Some(Box::new(move |url: &str| {
                let _ = url_tx.send(url.to_string());
            })),
            stop: Some(stop_rx),
            kernelspecs: Some(app.clone()),
            provisioner: Some(app.clone()),
            transports: Some(app),
            ..Default::default()
        },
    ));
    let url = url_rx.await.unwrap();
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();

    let (code, specs) = call(addr, "GET", "/api/kernelspecs", None).await;
    assert_eq!(code, 200);
    assert_eq!(specs["default"], "local", "{specs}");
    // A registered kernel has no directory, so the key is left out.
    let local = specs["kernelspecs"]["local"].as_object().unwrap();
    assert!(!local.contains_key("resource_dir"), "{specs}");
    let (code, one) = call(addr, "GET", "/api/kernelspecs/local", None).await;
    assert_eq!(code, 200);
    assert!(one.get("resource_dir").is_none(), "{one}");
    let (code, kernel) = call(addr, "POST", "/api/kernels", Some(json!({"name": "local"}))).await;
    assert_eq!(code, 201, "{kernel}");
    let path = format!("/api/kernels/{}/restart", kernel["id"].as_str().unwrap());
    let (code, _) = call(addr, "POST", &path, None).await;
    assert_eq!(code, 200);

    stop_tx.send(()).await.unwrap();
    server.await.unwrap().unwrap();
}

/// The Jupyter runtime dir for this test run's servers: a temp dir, set
/// once before any of them starts, so their rooms lock there and not in
/// the real one (`~/Library/Jupyter/runtime`). Set once and never changed,
/// so parallel tests can't race it; each test's notebooks are in its own
/// temp root, so their locks never meet. The CLI tests, whose servers are
/// subprocesses, set `JUPYTER_RUNTIME_DIR` per command instead.
fn temp_runtime_dir() {
    static RUNTIME_DIR: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    RUNTIME_DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a temp runtime dir");
        // Kept, not deleted on drop: the servers' rooms lock in it while
        // this test binary runs.
        std::env::set_var("JUPYTER_RUNTIME_DIR", dir.keep());
    });
}

/// A server as an embedding app runs one (`stop` set, no output): on a
/// thread of its own with a tokio runtime of its own, as ganymede-ffi
/// runs it, serving `root`. Its rooms — and their locks — go with its
/// runtime: when the server stops, they are dropped, not left running
/// on this test's runtime to hold their locks. Returns its address, its
/// stop channel and its thread.
async fn embedded_server(
    root: &std::path::Path,
) -> (
    String,
    tokio::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = Config {
        port: 0,
        token: Some("t".into()),
        root_dir: root.into(),
        cookie_secret_file: Some(root.join("secret")),
        notary_dir: Some(root.join("notary")),
        ..Default::default()
    };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    let thread = std::thread::Builder::new()
        .name("embedded-server".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("embedded")
                .build()?;
            let startup = Startup {
                on_listening: Some(Box::new(move |url: &str| {
                    let _ = url_tx.send(url.to_string());
                })),
                stop: Some(stop_rx),
                kernelspecs: Some(Arc::new(App)),
                provisioner: Some(Arc::new(App)),
                transports: Some(Arc::new(App)),
                ..Default::default()
            };
            rt.block_on(run_with(cfg, startup))
        })
        .expect("the server thread");
    let url = url_rx.await.expect("the server to listen");
    (
        url.trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .to_string(),
        stop_tx,
        thread,
    )
}

/// Two servers started as an embedding app starts them (with `stop`, as
/// ganymede-ffi runs its servers) take room locks too: whichever opens a
/// room on a notebook first owns it and the other's room is read-only,
/// which its contents API says as `writable: false`. Both orders (#966).
#[tokio::test]
async fn an_embedded_server_locks_its_rooms_first_come() {
    use futures::{SinkExt, StreamExt};
    use ganymede_documents::schema::Roots;
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Text, Transact, Update};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    temp_runtime_dir();
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("shared.ipynb");
    let original = r#"{ "cells": [ { "cell_type": "markdown", "id": "a", "metadata": {}, "source": [ "hello" ] } ], "metadata": {}, "nbformat": 4, "nbformat_minor": 5 }
"#;
    std::fs::write(&file, original).unwrap();
    let (a_addr, a_stop, a_server) = embedded_server(root.path()).await;
    let (b_addr, b_stop, b_server) = embedded_server(root.path()).await;

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;
    // A room's websocket, opened through the session the server hands
    // out, as JupyterLab opens one.
    async fn room(addr: &str, path: &str) -> Ws {
        let (code, v) = call(
            addr,
            "PUT",
            &format!("/api/collaboration/session/{path}"),
            Some(json!({"format": "json", "type": "notebook"})),
        )
        .await;
        assert!((200..300).contains(&code), "{code}: {v}");
        let mut req = format!(
            "ws://{addr}/api/collaboration/room/json:notebook:{}?sessionId={}",
            v["fileId"].as_str().unwrap(),
            v["sessionId"].as_str().unwrap()
        )
        .into_client_request()
        .unwrap();
        req.headers_mut()
            .insert("Authorization", "token t".parse().unwrap());
        tokio_tungstenite::connect_async(req).await.unwrap().0
    }
    fn source(doc: &Doc) -> Option<String> {
        let roots = Roots::get(doc);
        let txn = doc.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            return None;
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            return None;
        };
        Some(ganymede_documents::yrs::GetString::get_string(&t, &txn))
    }
    // Apply frames until `until` holds, answering the server's step 1.
    async fn pump(ws: &mut Ws, doc: &Doc, until: impl Fn(&Doc) -> bool) {
        let sv = doc.transact().state_vector();
        ws.send(TMsg::Binary(
            YMsg::Sync(SyncMessage::SyncStep1(sv)).encode_v1().into(),
        ))
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !until(doc) {
            let msg = tokio::time::timeout_at(deadline, ws.next())
                .await
                .expect("timed out waiting for the room")
                .unwrap()
                .unwrap();
            let TMsg::Binary(b) = msg else { continue };
            match YMsg::decode_v1(&b) {
                Ok(YMsg::Sync(SyncMessage::SyncStep1(sv))) => {
                    let u = doc.transact().encode_state_as_update_v1(&sv);
                    ws.send(TMsg::Binary(
                        YMsg::Sync(SyncMessage::SyncStep2(u)).encode_v1().into(),
                    ))
                    .await
                    .unwrap();
                }
                Ok(YMsg::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u))) => {
                    doc.transact_mut()
                        .apply_update(Update::decode_v1(&u).unwrap())
                        .unwrap();
                }
                _ => {}
            }
        }
    }
    // Insert `text` at the end of the first cell's source and send it.
    async fn type_at_end(ws: &mut Ws, doc: &Doc, text: &str) {
        let before = doc.transact().state_vector();
        {
            let roots = Roots::get(doc);
            let mut txn = doc.transact_mut();
            let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
                panic!("no cell");
            };
            let t = match cell.get(&txn, "source") {
                Some(Out::YText(t)) => t,
                _ => panic!("no source"),
            };
            let end = t.len(&txn);
            t.insert(&mut txn, end, text);
        }
        let u = doc.transact().encode_state_as_update_v1(&before);
        ws.send(TMsg::Binary(
            YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
        ))
        .await
        .unwrap();
    }
    async fn writable(addr: &str) -> bool {
        let (code, m) = call(addr, "GET", "/api/contents/shared.ipynb?content=0", None).await;
        assert_eq!(code, 200, "{m}");
        m["writable"].as_bool().unwrap()
    }
    async fn saved(file: &std::path::Path, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !std::fs::read_to_string(file).unwrap().contains(what) {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} was not saved: {:?}",
                std::fs::read_to_string(file).unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // No room is open: both servers see the notebook as theirs to write.
    assert!(writable(&a_addr).await);
    assert!(writable(&b_addr).await);

    // Server A, embedded like B, opens the room first and owns the file.
    let (mut wsa, doca) = (room(&a_addr, "shared.ipynb").await, Doc::new());
    pump(&mut wsa, &doca, |d| source(d).is_some()).await;
    assert!(writable(&a_addr).await, "the owner can write");
    assert!(!writable(&b_addr).await, "the second server is read-only");
    let (mut wsb, docb) = (room(&b_addr, "shared.ipynb").await, Doc::new());
    pump(&mut wsb, &docb, |d| source(d).is_some()).await;
    // B's read-only room undoes its client's edit and saves nothing.
    type_at_end(&mut wsb, &docb, " from B").await;
    pump(&mut wsb, &docb, |d| source(d).as_deref() == Some("hello")).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    // A's room saves its own client's edit, and B's room shows it.
    type_at_end(&mut wsa, &doca, " from A").await;
    saved(&file, "hello from A").await;
    pump(&mut wsb, &docb, |d| source(d).as_deref() == Some("hello from A")).await;

    // The other order: A stops, its room's lock goes with it, and B's
    // room takes the file over. A fresh embedded server is read-only then.
    a_stop.send(()).await.unwrap();
    a_server.join().unwrap().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !writable(&b_addr).await {
        assert!(
            std::time::Instant::now() < deadline,
            "the owner's lock was never released"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // The room's retry tick is about a second; give it room before the
    // edit, so the edit doesn't land while the room is still read-only.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    type_at_end(&mut wsb, &docb, " and B").await;
    saved(&file, "hello from A and B").await;

    let (c_addr, c_stop, c_server) = embedded_server(root.path()).await;
    assert!(!writable(&c_addr).await, "the new server is read-only");
    assert!(writable(&b_addr).await, "the new owner can write");
    c_stop.send(()).await.unwrap();
    c_server.join().unwrap().unwrap();
    wsa.close(None).await.unwrap();
    wsb.close(None).await.unwrap();
    b_stop.send(()).await.unwrap();
    b_server.join().unwrap().unwrap();
}

#[cfg(not(feature = "subprocess"))]
#[tokio::test]
async fn without_subprocess_a_provisioner_is_required() {
    let root = tempfile::tempdir().unwrap();
    let cfg = Config {
        port: 0,
        root_dir: root.path().into(),
        cookie_secret_file: Some(root.path().join("secret")),
        notary_dir: Some(root.path().join("notary")),
        ..Default::default()
    };
    let e = run_with(cfg, Startup::default()).await.unwrap_err();
    assert!(e.to_string().contains("provisioner"), "{e}");
}

/// `[tailscale] allow` is refused without the `Host` check, which stops a
/// DNS-rebinding page from reaching the server through `tailscale serve`.
#[tokio::test]
async fn tailnet_logins_are_refused_with_remote_access_on() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        port: 0,
        root_dir: root.path().into(),
        cookie_secret_file: Some(root.path().join("secret")),
        notary_dir: Some(root.path().join("notary")),
        allow_remote_access: Some(true),
        ..Default::default()
    };
    cfg.tailscale.allow = vec!["me@example.com".into()];
    let e = run_with(cfg, Startup::default()).await.unwrap_err();
    assert!(e.to_string().contains("Host check"), "{e}");
}

/// With auth off, `[tailscale] allow` would do nothing; it is refused so
/// nobody expects it to keep others out.
#[tokio::test]
async fn tailnet_logins_are_refused_with_auth_off() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        port: 0,
        token: Some(String::new()),
        root_dir: root.path().into(),
        cookie_secret_file: Some(root.path().join("secret")),
        notary_dir: Some(root.path().join("notary")),
        ..Default::default()
    };
    cfg.tailscale.allow = vec!["me@example.com".into()];
    let e = run_with(cfg, Startup::default()).await.unwrap_err();
    assert!(e.to_string().contains("auth off"), "{e}");
}

/// `whois` needs the server on exactly its tailnet address, so it can
/// tell its own address (where local processes connect from) from peers'.
#[tokio::test]
async fn whois_needs_the_tailnet_address() {
    for ip in ["127.0.0.1", "0.0.0.0", "192.168.1.2"] {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            ip: ip.into(),
            port: 0,
            root_dir: root.path().into(),
            cookie_secret_file: Some(root.path().join("secret")),
            notary_dir: Some(root.path().join("notary")),
            ..Default::default()
        };
        cfg.tailscale.allow = vec!["me@example.com".into()];
        cfg.tailscale.whois = true;
        let e = run_with(cfg, Startup::default()).await.unwrap_err();
        assert!(e.to_string().contains("tailnet address"), "{ip}: {e}");
    }
}
