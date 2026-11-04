//! The REST and websocket surface against the fake kernel.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures::{SinkExt, StreamExt};
use ganymede_api::{
    KernelManager, KernelManagerConfig, SessionManager, StaticKernelspecs, TransportFactory,
};
use ganymede_contents::{FsContents, FsOptions};
use ganymede_kernel_host::protocol::{
    content::ExecuteRequest, ws::v1, Channel, ConnectionInfo, Message,
};
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    attach, in_process_pair, KernelTransport, Kernelspec, LaunchOptions, LaunchedKernel,
    Provisioner,
};
use ganymede_server::{build_router, AppState};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

struct FakeProvisioner;
#[async_trait]
impl Provisioner for FakeProvisioner {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(attach(ConnectionInfo::new_tcp(
            "127.0.0.1",
            [1, 2, 3, 4, 5],
        )))
    }
}
struct FakeTransports;
#[async_trait]
impl TransportFactory for FakeTransports {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(256);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

fn state(root: &std::path::Path) -> AppState {
    let spec = Kernelspec {
        name: "fake".into(),
        resource_dir: root.into(),
        argv: vec!["fake".into()],
        display_name: "Fake".into(),
        language: "fake".into(),
        interrupt_mode: ganymede_kernel_host::InterruptMode::Message,
        env: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    };
    let kernels = Arc::new(KernelManager::new(
        Arc::new(StaticKernelspecs(vec![spec])),
        Arc::new(FakeProvisioner),
        Arc::new(FakeTransports),
        KernelManagerConfig {
            startup_timeout: Duration::from_secs(5),
            root_dir: root.into(),
            ..Default::default()
        },
    ));
    AppState {
        sessions: Arc::new(SessionManager::new(kernels.clone())),
        kernels,
        contents: Arc::new(FsContents::new(root, FsOptions::default()).unwrap()),
        token: Some(TOKEN.into()),
        cookie_secret: Arc::from(&b"test-secret"[..]),
        allow_origins: vec![],
        allow_credentials: false,
        allow_remote_access: false,
        local_hostnames: vec!["localhost".into()],
        trust_xheaders: false,
        max_body_size: 512 * 1024 * 1024,
        base_url: "/".into(),
        config_dir: root.join(".jupyter-config"),
        lab: None,
        static_dir: None,
        collab: Arc::new(ganymede_server::collab::Collab::new(
            ganymede_contents::FileIds::in_memory(),
            root.canonicalize().ok(),
        )),
        terminals: None,
        lsp: None,
        mcp: None,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("token {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, value)
}

#[tokio::test]
async fn auth_is_required_and_token_query_works() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    assert_eq!(
        call(&app, "GET", "/api/status", None, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/status", None, Some("wrong"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/status", None, Some(TOKEN)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/status?token={TOKEN}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn kernelspecs_kernels_and_sessions_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    let (st, specs) = call(&app, "GET", "/api/kernelspecs", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(specs["default"], "fake");
    assert_eq!(specs["kernelspecs"]["fake"]["spec"]["display_name"], "Fake");
    assert_eq!(
        call(&app, "GET", "/api/kernelspecs/nope", None, t).await.0,
        StatusCode::NOT_FOUND
    );

    // JupyterLab's client sends JSON bodies with no Content-Type header.
    let req = Request::builder()
        .method("POST")
        .uri("/api/kernels")
        .header(header::AUTHORIZATION, format!("token {TOKEN}"))
        .body(Body::from(json!({"name": "fake"}).to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "JSON body without Content-Type is accepted"
    );
    let k: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = k["id"].as_str().unwrap().to_string();
    // Answered once the process runs, before its first reply, as
    // jupyter_server does.
    assert_eq!(k["execution_state"], "starting");
    let (_, list) = call(&app, "GET", "/api/kernels", None, t).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/kernels/{id}/interrupt"),
            None,
            t
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (st, r) = call(&app, "POST", &format!("/api/kernels/{id}/restart"), None, t).await;
    assert_eq!((st, r["id"].as_str()), (StatusCode::OK, Some(id.as_str())));

    let (st, s) = call(
        &app,
        "POST",
        "/api/sessions",
        Some(json!({"path": "nb.ipynb", "type": "notebook", "kernel": {"id": id}})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{s}");
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["kernel"]["id"], id);
    assert_eq!(s["notebook"]["path"], "nb.ipynb");
    let (st, p) = call(
        &app,
        "PATCH",
        &format!("/api/sessions/{sid}"),
        Some(json!({"path": "renamed.ipynb"})),
        t,
    )
    .await;
    assert_eq!(
        (st, p["path"].as_str()),
        (StatusCode::OK, Some("renamed.ipynb"))
    );
    assert_eq!(
        call(&app, "DELETE", &format!("/api/sessions/{sid}"), None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", &format!("/api/kernels/{id}"), None, t)
            .await
            .0,
        StatusCode::NOT_FOUND,
        "session delete shuts the kernel down"
    );

    let (_, status) = call(&app, "GET", "/api/status", None, t).await;
    assert_eq!(status["kernels"], 0);
}

#[tokio::test]
async fn contents_rest_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    let (st, root) = call(&app, "GET", "/api/contents", None, t).await;
    assert_eq!(
        (st, root["type"].as_str()),
        (StatusCode::OK, Some("directory"))
    );
    assert_eq!(root["content"].as_array().unwrap().len(), 0);

    let (st, d) = call(
        &app,
        "PUT",
        "/api/contents/sub",
        Some(json!({"type": "directory"})),
        t,
    )
    .await;
    assert_eq!(
        (st, d["type"].as_str()),
        (StatusCode::CREATED, Some("directory")),
        "{d}"
    );

    let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code", "id": "c1", "source": "1+1", "metadata": {}, "outputs": [], "execution_count": null}]});
    let (st, saved) = call(
        &app,
        "PUT",
        "/api/contents/sub/a.ipynb",
        Some(json!({"type": "notebook", "format": "json", "content": nb})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{saved}");
    assert_eq!(saved["path"], "sub/a.ipynb");
    let (st, _) = call(
        &app,
        "PUT",
        "/api/contents/sub/a.ipynb",
        Some(json!({"type": "notebook", "format": "json", "content": nb})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "second save is 200");

    let (st, got) = call(&app, "GET", "/api/contents/sub/a.ipynb", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(got["content"]["cells"][0]["source"], "1+1");
    let (_, meta) = call(&app, "GET", "/api/contents/sub/a.ipynb?content=0", None, t).await;
    assert!(meta["content"].is_null());

    let (st, cp) = call(
        &app,
        "POST",
        "/api/contents/sub/a.ipynb/checkpoints",
        None,
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{cp}");
    let (_, cps) = call(
        &app,
        "GET",
        "/api/contents/sub/a.ipynb/checkpoints",
        None,
        t,
    )
    .await;
    assert_eq!(cps.as_array().unwrap().len(), 1);

    let (st, new) = call(
        &app,
        "POST",
        "/api/contents/sub",
        Some(json!({"type": "file", "ext": ".txt"})),
        t,
    )
    .await;
    assert_eq!(
        (st, new["path"].as_str()),
        (StatusCode::CREATED, Some("sub/untitled.txt"))
    );
    let (st, renamed) = call(
        &app,
        "PATCH",
        "/api/contents/sub/untitled.txt",
        Some(json!({"path": "sub/notes.txt"})),
        t,
    )
    .await;
    assert_eq!(
        (st, renamed["path"].as_str()),
        (StatusCode::OK, Some("sub/notes.txt"))
    );
    assert_eq!(
        call(&app, "DELETE", "/api/contents/sub/notes.txt", None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", "/api/contents/../etc", None, t).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", "/api/contents/missing.txt", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// JupyterLab uploads large files in chunks 1, 2, ..., -1; #52.
#[tokio::test]
async fn chunked_upload_appends_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);
    let mut statuses = vec![];
    for (chunk, part) in [(1, "part1;"), (2, "part2;"), (-1, "part-1;")] {
        let (st, model) = call(
            &app,
            "PUT",
            "/api/contents/up.bin",
            Some(json!({
                "type": "file", "format": "text", "content": part,
                "name": "up.bin", "path": "up.bin", "chunk": chunk
            })),
            t,
        )
        .await;
        assert_eq!(model["path"], "up.bin", "{model}");
        statuses.push(st);
    }
    assert_eq!(
        statuses,
        [StatusCode::CREATED, StatusCode::OK, StatusCode::OK]
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("up.bin")).unwrap(),
        "part1;part2;part-1;"
    );
}

#[tokio::test]
async fn contents_trust_route() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    st.contents = Arc::new(
        FsContents::new(
            dir.path(),
            FsOptions {
                notary: Some(Arc::new(
                    ganymede_contents::Notary::open(data.path()).unwrap(),
                )),
                ..FsOptions::default()
            },
        )
        .unwrap(),
    );
    let app = build_router(st);
    let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
        {"cell_type": "code", "id": "c", "metadata": {}, "source": "x", "execution_count": 1,
         "outputs": [{"output_type": "display_data", "metadata": {}, "data": {"text/html": "<b>x</b>"}}]}]});
    let (s, _) = call(
        &app,
        "PUT",
        "/api/contents/a.ipynb",
        Some(json!({"type": "notebook", "content": nb})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let trusted = |v: &Value| v["content"]["cells"][0]["metadata"]["trusted"].clone();
    let (_, v) = call(&app, "GET", "/api/contents/a.ipynb", None, Some(TOKEN)).await;
    assert_eq!(trusted(&v), json!(false));
    let (s, v) = call(
        &app,
        "POST",
        "/api/contents/a.ipynb/trust",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!((s, v), (StatusCode::CREATED, Value::Null));
    let (_, v) = call(&app, "GET", "/api/contents/a.ipynb", None, Some(TOKEN)).await;
    assert_eq!(trusted(&v), json!(true));
    let (s, _) = call(
        &app,
        "GET",
        "/api/contents/a.ipynb/trust",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    let (s, _) = call(
        &app,
        "POST",
        "/api/contents/nope.ipynb/trust",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn websocket_channels_v1_execute_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let k = kernels.start(Some("fake"), None).await.unwrap();

    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
    let mut req = format!("ws://{addr}/api/kernels/{}/channels?session_id=abc", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", v1::SUBPROTOCOL.parse().unwrap());
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(
        resp.headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok()),
        Some(v1::SUBPROTOCOL)
    );
    assert_eq!(kernels.model(&k.id).unwrap().connections, 1);

    let exec = Message::new(
        "execute_request",
        "client-sess",
        "me",
        &ExecuteRequest::new("ping"),
    );
    ws.send(TMsg::Binary(
        v1::encode(Channel::Shell, &exec).unwrap().into(),
    ))
    .await
    .unwrap();

    let mut got_reply = false;
    let mut got_idle = false;
    let mut stream_text = String::new();
    while !(got_reply && got_idle) {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("message in time")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = frame else { continue };
        let (channel, msg) = v1::decode(&b).unwrap();
        if msg.parent_id() != Some(exec.msg_id()) {
            continue;
        }
        match (channel, msg.msg_type()) {
            (Channel::Shell, "execute_reply") => got_reply = true,
            (Channel::IoPub, "status") if msg.content["execution_state"] == "idle" => {
                got_idle = true
            }
            (Channel::IoPub, "stream") => {
                stream_text.push_str(msg.content["text"].as_str().unwrap())
            }
            _ => {}
        }
    }
    assert_eq!(stream_text, "echo: ping\n");

    // Shutting the kernel down closes the socket from the server side.
    kernels.shutdown(&k.id).await.unwrap();
    let mut closed = false;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if let TMsg::Close(_) = frame {
            closed = true;
            break;
        }
    }
    assert!(closed, "server sent a close frame after shutdown");
}

/// Comm buffers through the websocket layer and back, byte for byte, in
/// both framings a frontend may use: the v1 subprotocol, and the legacy
/// binary frames of a client that asks for none (#68). The fake kernel's
/// `echo` comm sends every message back on iopub with its buffers.
#[tokio::test]
async fn websocket_carries_binary_buffers_both_ways() {
    use ganymede_kernel_host::protocol::ws::legacy;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let k = kernels.start(Some("fake"), None).await.unwrap();

    let all_bytes: Vec<u8> = (0..=255).collect();
    let large: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let buffers: Vec<axum::body::Bytes> = vec![
        // The ZMQ delimiter, which must stay data.
        axum::body::Bytes::from_static(b"<IDS|MSG>"),
        axum::body::Bytes::new(),
        all_bytes.into(),
        large.into(),
    ];

    for v1_protocol in [true, false] {
        let mut req = format!("ws://{addr}/api/kernels/{}/channels", k.id)
            .into_client_request()
            .unwrap();
        if v1_protocol {
            req.headers_mut()
                .insert("Sec-WebSocket-Protocol", v1::SUBPROTOCOL.parse().unwrap());
        }
        req.headers_mut()
            .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        let frame = |msg: &Message| -> TMsg {
            if v1_protocol {
                TMsg::Binary(v1::encode(Channel::Shell, msg).unwrap().into())
            } else {
                match legacy::encode(Channel::Shell, msg).unwrap() {
                    legacy::Frame::Text(t) => TMsg::Text(t.into()),
                    legacy::Frame::Binary(b) => TMsg::Binary(b.into()),
                }
            }
        };
        let comm_id = format!("comm-{v1_protocol}");
        let open = Message::new(
            "comm_open",
            "client-sess",
            "me",
            &json!({"comm_id": comm_id, "target_name": "echo", "data": {}}),
        );
        ws.send(frame(&open)).await.unwrap();
        let sent = Message::new(
            "comm_msg",
            "client-sess",
            "me",
            &json!({"comm_id": comm_id, "data": {"n": 4}}),
        )
        .with_buffers(buffers.clone());
        ws.send(frame(&sent)).await.unwrap();

        let echoed = loop {
            let got = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("echo in time")
                .unwrap()
                .unwrap();
            let (channel, msg) = match got {
                TMsg::Binary(b) if v1_protocol => v1::decode(&b).unwrap(),
                TMsg::Binary(b) => legacy::decode_binary(&b).unwrap(),
                TMsg::Text(t) if !v1_protocol => legacy::decode_text(t.as_str()).unwrap(),
                _ => continue,
            };
            if channel == Channel::IoPub
                && msg.msg_type() == "comm_msg"
                && msg.parent_id() == Some(sent.msg_id())
            {
                break msg;
            }
        };
        assert_eq!(echoed.content["data"], json!({"n": 4}));
        assert_eq!(echoed.buffers.len(), buffers.len(), "v1: {v1_protocol}");
        for (i, (got, want)) in echoed.buffers.iter().zip(&buffers).enumerate() {
            assert!(got == want, "v1: {v1_protocol}: buffer {i} differs");
        }
    }
    kernels.shutdown(&k.id).await.unwrap();
}

/// #53: output produced while a session's websocket is down is replayed
/// when it reconnects with the same `session_id`, as in jupyter_server.
#[tokio::test]
async fn websocket_reconnect_replays_buffered_output() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let k = kernels.start(Some("fake"), None).await.unwrap();

    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
    let connect = || async {
        let mut req = format!("ws://{addr}/api/kernels/{}/channels?session_id=tab", k.id)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", v1::SUBPROTOCOL.parse().unwrap());
        req.headers_mut()
            .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
        tokio_tungstenite::connect_async(req).await.unwrap().0
    };

    let mut ws = connect().await;
    let exec = Message::new(
        "execute_request",
        "tab",
        "me",
        &ExecuteRequest::new("sleep 300"),
    );
    ws.send(TMsg::Binary(
        v1::encode(Channel::Shell, &exec).unwrap().into(),
    ))
    .await
    .unwrap();
    ws.close(None).await.unwrap();
    drop(ws);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(kernels.model(&k.id).unwrap().connections, 0);

    let mut ws = connect().await;
    let mut got_reply = false;
    while !got_reply {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("replayed reply in time")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = frame else { continue };
        let (channel, msg) = v1::decode(&b).unwrap();
        got_reply = channel == Channel::Shell
            && msg.msg_type() == "execute_reply"
            && msg.parent_id() == Some(exec.msg_id());
    }
}

#[tokio::test]
async fn websocket_rejects_bad_token_and_foreign_origin() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let k = kernels.start(Some("fake"), None).await.unwrap();

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let req = format!("ws://{addr}/api/kernels/{}/channels", k.id)
        .into_client_request()
        .unwrap();
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "no token"
    );

    let mut req = format!("ws://{addr}/api/kernels/{}/channels?token={TOKEN}", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", "http://evil.example".parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "foreign origin"
    );

    let mut req = format!("ws://{addr}/api/kernels/{}/channels?token={TOKEN}", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", format!("http://{addr}").parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_ok(),
        "same-host origin with query token"
    );
}

#[tokio::test]
async fn server_info_identity_and_config() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    // `GET /api` is public in jupyter_server.
    let (st, v) = call(&app, "GET", "/api", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["version"].as_str().unwrap().starts_with("2."));

    let (st, me) = call(&app, "GET", "/api/me", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert!(me["identity"]["username"].is_string());
    assert_eq!(
        call(&app, "GET", "/api/me", None, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (st, c) = call(&app, "GET", "/api/config/notebook", None, t).await;
    assert_eq!((st, c), (StatusCode::OK, json!({})));
    let (_, c) = call(
        &app,
        "PATCH",
        "/api/config/notebook",
        Some(json!({"a": {"b": 1, "c": 2}, "d": 3})),
        t,
    )
    .await;
    assert_eq!(c, json!({"a": {"b": 1, "c": 2}, "d": 3}));
    let (_, c) = call(
        &app,
        "PATCH",
        "/api/config/notebook",
        Some(json!({"a": {"c": null}, "d": null})),
        t,
    )
    .await;
    assert_eq!(c, json!({"a": {"b": 1}}));
    let (_, c) = call(&app, "GET", "/api/config/notebook", None, t).await;
    assert_eq!(c, json!({"a": {"b": 1}}), "persisted");
    // Concurrent PATCHes are serialized: none loses another's key.
    let patches = (0..16).map(|i| {
        call(
            &app,
            "PATCH",
            "/api/config/many",
            Some(json!({ format!("k{i}"): i })),
            t,
        )
    });
    futures::future::join_all(patches).await;
    let (_, c) = call(&app, "GET", "/api/config/many", None, t).await;
    assert_eq!(c.as_object().unwrap().len(), 16, "{c}");
    assert_eq!(
        call(&app, "GET", "/api/config/..%2Fescape", None, t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    let (st, nb) = call(&app, "GET", "/api/nbconvert", None, t).await;
    assert_eq!((st, nb), (StatusCode::OK, json!({})));
}

#[tokio::test]
async fn kernelspec_resources_are_listed_and_served() {
    let dir = tempfile::tempdir().unwrap();
    // The fake spec's resource dir is the root.
    std::fs::write(dir.path().join("logo-64x64.png"), b"png").unwrap();
    std::fs::write(dir.path().join("kernel.json"), b"{}").unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None, t).await;
    assert_eq!(
        specs["kernelspecs"]["fake"]["resources"],
        json!({"logo-64x64": "/kernelspecs/fake/logo-64x64.png"})
    );
    let (st, body) = call(&app, "GET", "/kernelspecs/fake/logo-64x64.png", None, t).await;
    assert_eq!((st, body), (StatusCode::OK, json!("png")));
    assert_eq!(
        call(&app, "GET", "/kernelspecs/fake/missing.png", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", "/kernelspecs/nope/logo-64x64.png", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn events_websocket_accepts_authenticated_clients() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/events/subscribe"))
            .await
            .is_err(),
        "no token"
    );
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/events/subscribe?token={TOKEN}"))
            .await
            .unwrap();
    ws.close(None).await.unwrap();
}

// ----- JupyterLab --------------------------------------------------------------

/// A miniature JupyterLab application directory with the packaged
/// template's constructs, one core schema, one theme, and one prebuilt
/// extension with its own schema.
fn fake_lab(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let app = root.join("app");
    let w = |p: std::path::PathBuf, s: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    };
    w(
        app.join("static/index.html"),
        "<html><head>{# c #} {% set _ = x %}<script id=\"jupyter-config-data\" type=\"application/json\">{{ page_config_full | tojson }}</script>{% block favicon %}<link href=\"{{ base_url | escape }}static/favicons/favicon.ico\">{% endblock %}<script src=\"{{page_config.fullStaticUrl}}/main.js\"></script></head></html>",
    );
    w(
        app.join("static/package.json"),
        r#"{"name": "@jupyterlab/application-top", "jupyterlab": {"name": "JupyterLab", "version": "4.9.9"}}"#,
    );
    w(app.join("static/main.js"), "main");
    w(
        app.join("static/third-party-licenses.json"),
        r#"{"packages": [{"name": "x", "extractedText": "long"}]}"#,
    );
    w(
        app.join("schemas/@jupyterlab/apputils-extension/themes.json"),
        r#"{"title": "Theme", "properties": {"theme": {"type": "string", "default": "JupyterLab Light"}}}"#,
    );
    w(
        app.join("schemas/@jupyterlab/apputils-extension/package.json.orig"),
        r#"{"version": "4.9.8"}"#,
    );
    w(
        app.join("themes/@jupyterlab/theme-light-extension/index.css"),
        "body{}",
    );
    let ext = root.join("labextensions");
    w(
        ext.join("my_ext/package.json"),
        r#"{"name": "my_ext", "version": "0.1.0", "jupyterlab": {"_build": {"load": "static/remoteEntry.abc.js", "extension": "./extension"}}}"#,
    );
    w(ext.join("my_ext/static/remoteEntry.abc.js"), "remote");
    w(
        ext.join("my_ext/schemas/my_ext/plugin.json"),
        r#"{"title": "Mine"}"#,
    );
    (app.join("static"), ext)
}

fn lab_state(root: &std::path::Path) -> AppState {
    let (static_dir, ext) = fake_lab(root);
    let mut st = state(root);
    st.lab =
        ganymede_server::lab::Lab::detect(&static_dir, &[ext], &st.config_dir, root).map(Arc::new);
    assert!(st.lab.is_some(), "fake app dir is recognised");
    st
}

async fn get_text(app: &axum::Router, path: &str) -> (StatusCode, String) {
    get_text_with(app, path, &[]).await
}

async fn get_text_with(
    app: &axum::Router,
    path: &str,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, String) {
    let mut req = Request::get(path);
    for (k, v) in headers {
        req = req.header(k, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn lab_page_carries_page_config_and_assets_are_public() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));

    let resp = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.headers()[header::LOCATION], "/lab");

    let (st, html) = get_text_with(
        &app,
        "/lab/tree/a/b.ipynb",
        &[(header::AUTHORIZATION, &format!("token {TOKEN}"))],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        html.contains("<script src=\"/static/lab/main.js\">"),
        "{html}"
    );
    assert!(html.contains("<link href=\"/static/favicons/favicon.ico\">"));
    assert!(!html.contains("{%") && !html.contains("{{") && !html.contains("{#"));
    let json = html
        .split("type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let cfg: Value = serde_json::from_str(json).unwrap();
    assert_eq!(cfg["appVersion"], "4.9.9");
    assert_eq!(cfg["token"], TOKEN);
    assert_eq!(cfg["treePath"], "a/b.ipynb");
    assert_eq!(cfg["terminalsAvailable"], false);
    let root_uri = cfg["rootUri"].as_str().unwrap();
    assert!(root_uri.starts_with("file://"), "{root_uri}");
    assert_eq!(
        cfg["virtualDocumentsUri"],
        format!("{root_uri}/.virtual_documents")
    );
    assert_eq!(
        cfg["federated_extensions"],
        json!([{"name": "my_ext", "load": "static/remoteEntry.abc.js", "entrypoints": null, "extension": "./extension"}])
    );
    let (_, html) = get_text_with(
        &app,
        "/lab/workspaces/w1",
        &[(header::AUTHORIZATION, &format!("token {TOKEN}"))],
    )
    .await;
    assert!(html.contains("\"workspace\":\"w1\""));

    assert_eq!(
        get_text(&app, "/static/lab/main.js").await,
        (StatusCode::OK, "main".into())
    );
    assert_eq!(
        get_text(
            &app,
            "/lab/api/themes/@jupyterlab/theme-light-extension/index.css"
        )
        .await,
        (StatusCode::OK, "body{}".into())
    );
    assert_eq!(
        get_text(&app, "/lab/extensions/my_ext/static/remoteEntry.abc.js").await,
        (StatusCode::OK, "remote".into())
    );
    assert_eq!(
        get_text(&app, "/lab/api/themes/../static/main.js").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn lab_extensions_are_listed_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let rich = dir.path().join("labextensions/@scope/rich");
    std::fs::create_dir_all(&rich).unwrap();
    std::fs::write(
        rich.join("package.json"),
        r#"{"name": "@scope/rich", "version": "2.0.0rc1", "description": "Rich",
            "author": {"name": "A. Author"}, "license": "MIT",
            "bugs": {"url": "https://example.com/bugs"},
            "repository": {"type": "git", "url": "https://example.com/repo"},
            "jupyterlab": {"_build": {"load": "static/remoteEntry.js"}, "discovery": {"kernel": []}}}"#,
    )
    .unwrap();
    std::fs::write(
        rich.join("install.json"),
        r#"{"packageManager": "python", "packageName": "rich"}"#,
    )
    .unwrap();
    let st = lab_state(dir.path());
    let config_dir = st.config_dir.clone();
    let app = build_router(st);
    let t = Some(TOKEN);
    assert_eq!(
        call(&app, "GET", "/lab/api/extensions", None, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (st, list) = call(&app, "GET", "/lab/api/extensions", None, t).await;
    assert_eq!(st, StatusCode::OK);
    let list = list.as_array().unwrap();
    let rich = list.iter().find(|e| e["name"] == "@scope/rich").unwrap();
    assert_eq!(rich["description"], "Rich");
    assert_eq!(rich["homepage_url"], "https://example.com/repo");
    assert_eq!(rich["repository_url"], "https://example.com/repo");
    assert_eq!(rich["bug_tracker_url"], "https://example.com/bugs");
    assert_eq!(rich["author"], "A. Author");
    assert_eq!(rich["license"], "MIT");
    assert_eq!(rich["companion"], "kernel");
    assert_eq!(rich["installed_version"], "2.0.0-rc.1");
    assert_eq!(rich["install"]["packageName"], "rich");
    let mine = list.iter().find(|e| e["name"] == "my_ext").unwrap();
    assert_eq!(
        mine,
        &json!({
            "name": "my_ext",
            "description": "",
            "homepage_url": "",
            "pkg_type": "prebuilt",
            "allowed": true,
            "approved": false,
            "companion": null,
            "core": false,
            "enabled": true,
            "install": {},
            "installed": true,
            "installed_version": "0.1.0",
            "latest_version": "0.1.0",
            "status": "ok",
            "author": null,
            "license": null,
            "bug_tracker_url": null,
            "documentation_url": null,
            "package_manager_url": null,
            "repository_url": null,
        })
    );

    // Disabled in page_config.json, as `ganymede lab extension disable` writes it.
    let pc = config_dir.join("labconfig/page_config.json");
    std::fs::create_dir_all(pc.parent().unwrap()).unwrap();
    std::fs::write(&pc, r#"{"disabledExtensions": {"my_ext": true}}"#).unwrap();
    let (_, list) = call(&app, "GET", "/lab/api/extensions", None, t).await;
    let enabled = |n: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == n)
            .unwrap()["enabled"]
            .clone()
    };
    assert_eq!(
        (enabled("my_ext"), enabled("@scope/rich")),
        (json!(false), json!(true))
    );

    // A search finds nothing: there is no package index.
    let (st, h, list) = call_with(
        &app,
        "GET",
        "/lab/api/extensions?query=widgets&page=1&per_page=20",
        None,
        &[(header::AUTHORIZATION, &format!("token {TOKEN}"))],
    )
    .await;
    assert_eq!((st, list), (StatusCode::OK, json!([])));
    assert_eq!(
        h[header::LINK],
        "</lab/api/extensions?page=1&per_page=20>; rel=\"last\", \
         </lab/api/extensions?page=1&per_page=20>; rel=\"first\""
    );

    let (st, body) = call(
        &app,
        "POST",
        "/lab/api/extensions",
        Some(json!({"cmd": "install", "extension_name": "x"})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["status"], "error");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("ganymede lab extension"));
}

#[tokio::test]
async fn lab_settings_round_trip_json5() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));
    let t = Some(TOKEN);
    assert_eq!(
        call(&app, "GET", "/lab/api/settings", None, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (_, ids) = call(&app, "GET", "/lab/api/settings?ids_only=true", None, t).await;
    assert_eq!(
        ids,
        json!({"settings": [{"id": "@jupyterlab/apputils-extension:themes"}, {"id": "my_ext:plugin"}]})
    );

    let id = "@jupyterlab/apputils-extension:themes";
    let (st, s) = call(&app, "GET", &format!("/lab/api/settings/{id}"), None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["raw"], "{}");
    assert_eq!(s["version"], "4.9.8");
    assert_eq!(s["schema"]["title"], "Theme");

    let raw = "{\n  // a comment\n  \"theme\": \"JupyterLab Dark\",\n}";
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/lab/api/settings/{id}"),
        Some(json!({ "raw": raw })),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s) = call(&app, "GET", &format!("/lab/api/settings/{id}"), None, t).await;
    assert_eq!(s["raw"], raw);
    assert_eq!(s["settings"], json!({"theme": "JupyterLab Dark"}));
    assert!(s["last_modified"].is_string());

    let (_, all) = call(&app, "GET", "/lab/api/settings", None, t).await;
    assert_eq!(all["settings"][0]["settings"]["theme"], "JupyterLab Dark");
    assert_eq!(all["settings"][1]["version"], "0.1.0");

    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/lab/api/settings/{id}"),
            Some(json!({"raw": "{nope"})),
            t
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "GET", "/lab/api/settings/nope:nope", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn lab_workspaces_translations_and_listings() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));
    let t = Some(TOKEN);

    let (_, ws) = call(&app, "GET", "/lab/api/workspaces/default", None, t).await;
    assert_eq!(ws, json!({"data": {}, "metadata": {"id": "default"}}));
    let body =
        json!({"data": {"layout-restorer:data": {"main": {}}}, "metadata": {"id": "default"}});
    assert_eq!(
        call(&app, "PUT", "/lab/api/workspaces/default", Some(body), t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, ws) = call(&app, "GET", "/lab/api/workspaces/default", None, t).await;
    assert_eq!(ws["data"]["layout-restorer:data"], json!({"main": {}}));
    let (_, list) = call(&app, "GET", "/lab/api/workspaces", None, t).await;
    assert_eq!(list["workspaces"]["ids"], json!(["default"]));
    assert_eq!(
        call(&app, "PUT", "/lab/api/workspaces/x", Some(json!({})), t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "DELETE", "/lab/api/workspaces/default", None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "DELETE", "/lab/api/workspaces/default", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    let (_, tr) = call(&app, "GET", "/lab/api/translations", None, t).await;
    assert_eq!(tr["data"]["en"]["displayName"], "English");
    let (_, tr) = call(&app, "GET", "/lab/api/translations/default", None, t).await;
    assert_eq!(tr, json!({"data": {}, "message": ""}));

    let (st, l) = call(
        &app,
        "GET",
        "/lab/api/listings/@jupyterlab/extensionmanager-extension/listings.json",
        None,
        t,
    )
    .await;
    assert_eq!(
        (st, l["blocked_extensions"].clone()),
        (StatusCode::OK, json!([]))
    );
    let (_, p) = call(&app, "GET", "/lab/api/plugins", None, t).await;
    assert_eq!(p, json!({"lockRules": [], "allLocked": false}));
    let (_, lic) = call(&app, "GET", "/lab/api/licenses", None, t).await;
    assert_eq!(
        lic,
        json!({"bundles": {"@jupyterlab/application-top": {"packages": [{"name": "x"}]}}})
    );
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, header::HeaderMap, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// `name=value` of each `Set-Cookie`, and the whole header by name.
fn set_cookies(h: &header::HeaderMap) -> Vec<(String, String, String)> {
    h.get_all(header::SET_COOKIE)
        .iter()
        .map(|v| {
            let v = v.to_str().unwrap();
            let (nv, _) = v.split_once(';').unwrap_or((v, ""));
            let (n, val) = nv.split_once('=').unwrap();
            (n.to_string(), val.to_string(), v.to_string())
        })
        .collect()
}

const HOST: &str = "127.0.0.1:8888";
const LOGIN: &str = "username-127-0-0-1-8888";

fn get_req(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = Request::get(path).header(header::HOST, HOST);
    if let Some(c) = cookie {
        r = r.header(header::COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn token_in_a_page_url_becomes_a_login_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));

    let (st, h, _) = send(&app, get_req("/lab", None)).await;
    assert_eq!(st, StatusCode::FOUND);
    assert_eq!(h[header::LOCATION], "/login?next=%2Flab");

    let (st, h, _) = send(
        &app,
        get_req(&format!("/lab/tree/a.ipynb?token={TOKEN}&reset"), None),
    )
    .await;
    assert_eq!(st, StatusCode::FOUND);
    assert_eq!(h[header::LOCATION], "/lab/tree/a.ipynb?reset=");
    let cookies = set_cookies(&h);
    assert_eq!(cookies.len(), 1);
    let (name, value, full) = &cookies[0];
    assert_eq!(name, LOGIN);
    assert!(value.starts_with("2|1:0|"), "{full}");
    assert!(full.contains("; HttpOnly; Path=/; SameSite=Lax"), "{full}");
    let login = format!("{name}={value}");

    // A wrong token is not stripped; the page sends it to the login form.
    let (st, h, _) = send(&app, get_req("/lab?token=wrong", None)).await;
    assert_eq!(st, StatusCode::FOUND);
    assert_eq!(h[header::LOCATION], "/login?next=%2Flab%3Ftoken%3Dwrong");

    // The page with the cookie, which also gets an _xsrf cookie.
    let (st, h, html) = send(&app, get_req("/lab", Some(&login))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(html.contains("jupyter-config-data"));
    let cookies = set_cookies(&h);
    assert_eq!(cookies[0].0, "_xsrf");
    assert!(!cookies[0].2.contains("HttpOnly"));

    // `/` keeps redirecting to the app, after taking the token.
    let (_, h, _) = send(&app, get_req(&format!("/?token={TOKEN}"), None)).await;
    assert_eq!(h[header::LOCATION], "/");
    assert_eq!(set_cookies(&h)[0].0, LOGIN);

    // API URLs keep the token and only gain a cookie.
    let (st, h, _) = send(&app, get_req(&format!("/api/status?token={TOKEN}"), None)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(set_cookies(&h)[0].0, LOGIN);
    let (st, h, _) = send(
        &app,
        get_req(&format!("/api/status?token={TOKEN}"), Some(&login)),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(set_cookies(&h).is_empty(), "already logged in");
}

/// A generic `--static-dir` app (not JupyterLab) gets an `_xsrf` cookie
/// with its pages, so after the token-to-cookie redirect it can still
/// change state.
#[tokio::test]
async fn generic_static_dir_pages_get_xsrf() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("index.html"), "<!doctype html><title>app</title>").unwrap();
    std::fs::write(site.join("app.js"), "app").unwrap();
    let mut st = state(dir.path());
    st.static_dir = Some(site);
    let app = build_router(st);

    let (_, h, _) = send(&app, get_req(&format!("/?token={TOKEN}"), None)).await;
    assert_eq!(h[header::LOCATION], "/");
    let (n, v, _) = set_cookies(&h).remove(0);
    let login = format!("{n}={v}");

    let (st, h, html) = send(&app, get_req("/", Some(&login))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(html.contains("<title>app</title>"));
    let cookies = set_cookies(&h);
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].0, "_xsrf");
    let xsrf = cookies[0].1.clone();

    let (st, h, _) = send(&app, get_req("/app.js", Some(&login))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(set_cookies(&h).is_empty(), "not a page");
    let both = format!("{login}; _xsrf={xsrf}");
    let (_, h, _) = send(&app, get_req("/", Some(&both))).await;
    assert!(set_cookies(&h).is_empty(), "already has one");

    let req = Request::post("/api/contents")
        .header(header::HOST, HOST)
        .header(header::COOKIE, &both)
        .header("X-XSRFToken", &xsrf)
        .body(Body::from(r#"{"type": "file"}"#))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn cookie_auth_needs_xsrf_for_changes() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));
    let (_, h, _) = send(&app, get_req(&format!("/api/status?token={TOKEN}"), None)).await;
    let (n, v, _) = set_cookies(&h).remove(0);
    let login = format!("{n}={v}");
    let (_, h, _) = send(&app, get_req("/lab", Some(&login))).await;
    let (_, xsrf, _) = set_cookies(&h).remove(0);
    let both = format!("{login}; _xsrf={xsrf}");

    let (st, h, _) = send(&app, get_req("/api/kernelspecs", Some(&login))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(set_cookies(&h).is_empty());

    let post = |cookie: &str, extra: Option<(&str, &str)>| {
        let mut r = Request::post("/api/contents")
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie);
        if let Some((k, v)) = extra {
            r = r.header(k, v);
        }
        r.body(Body::from(r#"{"type": "file"}"#)).unwrap()
    };
    let (st, _, body) = send(&app, post(&both, None)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(
        body.contains("'_xsrf' argument missing from POST"),
        "{body}"
    );
    let (st, _, body) = send(&app, post(&both, Some(("X-XSRFToken", "not-the-token")))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(body.contains("XSRF cookie does not match"), "{body}");
    let (st, _, _) = send(&app, post(&login, Some(("X-XSRFToken", &xsrf)))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no _xsrf cookie");
    let (st, _, _) = send(&app, post(&both, Some(("X-XSRFToken", &xsrf)))).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _, _) = send(&app, post(&both, Some(("Origin", "http://evil.example")))).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "cross-origin");
    let mut cross = get_req("/api/status", Some(&login));
    cross
        .headers_mut()
        .insert("Origin", "http://evil.example".parse().unwrap());
    assert_eq!(send(&app, cross).await.0, StatusCode::NOT_FOUND);

    // The token needs no XSRF token.
    let (st, _) = call(
        &app,
        "POST",
        "/api/contents",
        Some(json!({"type": "file"})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // A forged login cookie is refused and cleared.
    let (st, h, _) = send(
        &app,
        get_req("/api/status", Some(&format!("{LOGIN}=2|1:0|x"))),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (n, v, _) = set_cookies(&h).remove(0);
    assert_eq!((n.as_str(), v.as_str()), (LOGIN, "\"\""));
}

#[tokio::test]
async fn login_form_and_logout() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));

    let (st, h, html) = send(&app, get_req("/login?next=%2Flab%2Ftree", None)).await;
    assert_eq!(st, StatusCode::OK);
    let (_, xsrf, _) = set_cookies(&h).remove(0);
    assert!(
        html.contains(&format!("name=\"_xsrf\" value=\"{xsrf}\"")),
        "{html}"
    );
    assert!(
        html.contains("action=\"/login?next=%2Flab%2Ftree\""),
        "{html}"
    );

    let form = |cookie: &str, body: String| {
        Request::post("/login?next=%2Flab%2Ftree")
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    };
    let xsrf_cookie = format!("_xsrf={xsrf}");
    let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let (st, h, _) = send(&app, form(&xsrf_cookie, format!("password={TOKEN}"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no _xsrf field");
    assert!(set_cookies(&h).is_empty());
    let (st, _, html) = send(
        &app,
        form(&xsrf_cookie, format!("password=bad&_xsrf={}", enc(&xsrf))),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(html.contains("Invalid credentials"));
    let (st, h, _) = send(
        &app,
        form(
            &xsrf_cookie,
            format!("password={TOKEN}&_xsrf={}", enc(&xsrf)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FOUND);
    assert_eq!(h[header::LOCATION], "/lab/tree");
    let (n, v, _) = set_cookies(&h).remove(0);
    assert_eq!(n, LOGIN);
    let login = format!("{n}={v}");

    // Logged in, the form sends the browser on; never off this server.
    let (st, h, _) = send(&app, get_req("/login?next=%2Flab", Some(&login))).await;
    assert_eq!(
        (st, &h[header::LOCATION]),
        (StatusCode::FOUND, &"/lab".parse().unwrap())
    );
    for evil in ["http://evil.example/", "//evil.example/", "/\\evil.example"] {
        let (_, h, _) = send(
            &app,
            get_req(&format!("/login?next={}", enc(evil)), Some(&login)),
        )
        .await;
        assert_eq!(h[header::LOCATION], "/", "{evil}");
    }

    let (st, h, html) = send(&app, get_req("/logout", Some(&login))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(html.contains("Successfully logged out."));
    let (n, v, full) = set_cookies(&h).remove(0);
    assert_eq!((n.as_str(), v.as_str()), (LOGIN, "\"\""));
    assert!(full.contains("Path=/"));
}

#[tokio::test]
async fn login_redirects_keep_the_base_url() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = lab_state(dir.path());
    st.base_url = "/b/".into();
    let app = axum::Router::new().nest("/b", build_router(st));
    let (_, h, _) = send(&app, get_req("/b/lab/tree/x?a=1", None)).await;
    assert_eq!(
        h[header::LOCATION],
        "/b/login?next=%2Fb%2Flab%2Ftree%2Fx%3Fa%3D1"
    );
    let (_, h, _) = send(&app, get_req(&format!("/b/lab?token={TOKEN}"), None)).await;
    assert_eq!(h[header::LOCATION], "/b/lab");
    assert!(set_cookies(&h)[0].2.contains("Path=/b/"));
}

#[tokio::test]
async fn websocket_authenticates_with_the_login_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    let k = kernels.start(Some("fake"), None).await.unwrap();

    let mut req = Request::get(format!("/api/status?token={TOKEN}"))
        .body(Body::empty())
        .unwrap();
    req.headers_mut()
        .insert(header::HOST, addr.to_string().parse().unwrap());
    let (_, h, _) = send(&app, req).await;
    let (n, v, _) = set_cookies(&h).remove(0);

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/api/kernels/{}/channels", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Cookie", format!("{n}={v}").parse().unwrap());
    req.headers_mut()
        .insert("Origin", format!("http://{addr}").parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_ok(),
        "cookie only, same origin"
    );
}

#[tokio::test]
async fn auth_off_still_needs_xsrf_to_change_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = lab_state(dir.path());
    st.token = None;
    let app = build_router(st);

    // A cross-site "simple" request: text/plain, no XSRF token.
    let post = |extra: &[(&str, &str)]| {
        let mut r = Request::post("/api/contents")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "text/plain");
        for (k, v) in extra {
            r = r.header(*k, *v);
        }
        r.body(Body::from(r#"{"type": "notebook"}"#)).unwrap()
    };
    let (st, _, body) = send(&app, post(&[("Origin", "https://evil.example")])).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "cross-origin: {body}");
    let (st, _, body) = send(&app, post(&[])).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("'_xsrf' argument missing"), "{body}");

    // Reads need nothing; the page hands out the cookie that writes need.
    let (st, _, _) = send(&app, get_req("/api/contents", None)).await;
    assert_eq!(st, StatusCode::OK);
    let (_, h, _) = send(&app, get_req("/lab", None)).await;
    let (_, xsrf, _) = set_cookies(&h).remove(0);
    let cookie = format!("_xsrf={xsrf}");
    let (st, _, body) = send(&app, post(&[("Cookie", &cookie), ("X-XSRFToken", &xsrf)])).await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn non_local_host_is_refused_on_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    st.local_hostnames.push("notebook.test".into());
    let app = build_router(st.clone());
    let status = |app: axum::Router, host: &'static str| async move {
        let req = Request::get(format!("/api/status?token={TOKEN}"))
            .header(header::HOST, host)
            .header(header::ORIGIN, format!("http://{host}"))
            .body(Body::empty())
            .unwrap();
        send(&app, req).await.0
    };
    for host in [
        "127.0.0.1:8888",
        "127.0.0.2",
        "localhost:8888",
        "LOCALHOST",
        "[::1]:8888",
        "[::1]",
        "notebook.test:8888",
    ] {
        assert_eq!(status(app.clone(), host).await, StatusCode::OK, "{host}");
    }
    for host in [
        "rebind.evil.example:8888",
        "rebind.evil.example",
        "10.0.0.5:8888",
        "[::2]:8888",
        "[::1]x",
        "localhost.evil.example",
    ] {
        assert_eq!(
            status(app.clone(), host).await,
            StatusCode::FORBIDDEN,
            "{host}"
        );
    }
    // Pages are covered too, not only the API.
    let req = Request::get("/login")
        .header(header::HOST, "rebind.evil.example:8888")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::FORBIDDEN);

    st.allow_remote_access = true;
    let app = build_router(st);
    assert_eq!(
        status(app, "rebind.evil.example:8888").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn notebooks_over_two_megabytes_save_and_the_limit_gives_413() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    let app = build_router(st.clone());
    // One 3 MB image output, past axum's 2 MB default.
    let png = "A".repeat(3 << 20);
    let nb = json!({
        "type": "notebook",
        "content": {
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [{
                "cell_type": "code", "id": "big", "metadata": {},
                "execution_count": 1, "source": "plot()",
                "outputs": [{
                    "output_type": "display_data", "metadata": {},
                    "data": {"image/png": png}
                }]
            }]
        }
    });
    let (status, body) = call(
        &app,
        "PUT",
        "/api/contents/big.ipynb",
        Some(nb.clone()),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        std::fs::metadata(dir.path().join("big.ipynb"))
            .unwrap()
            .len()
            > 3 << 20
    );

    st.max_body_size = 1 << 20;
    let app = build_router(st);
    let (status, _) = call(
        &app,
        "PUT",
        "/api/contents/big.ipynb",
        Some(nb),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

fn api_req(method: &str, path: &str, body: Option<Value>) -> Request<Body> {
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::AUTHORIZATION, format!("token {TOKEN}"));
    match body {
        Some(b) => r.body(Body::from(b.to_string())).unwrap(),
        None => r.body(Body::empty()).unwrap(),
    }
}

/// Python's `json` accepts a lone surrogate escape, so JupyterLab can send
/// one in a notebook; it is saved as U+FFFD instead of failing with 400.
#[tokio::test]
async fn put_body_with_a_lone_surrogate_saves() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let body = r#"{"type": "notebook", "format": "json", "content": {"nbformat": 4,
        "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "markdown",
        "id": "m", "metadata": {}, "source": "lone \ud800 here"}]}}"#;
    let mut req = api_req("PUT", "/api/contents/s.ipynb", None);
    *req.body_mut() = Body::from(body);
    let (s, _, text) = send(&app, req).await;
    assert_eq!(s, StatusCode::CREATED, "{text}");
    let (_, v) = call(&app, "GET", "/api/contents/s.ipynb", None, Some(TOKEN)).await;
    assert_eq!(v["content"]["cells"][0]["source"], "lone \u{fffd} here");
    assert!(v.get("message").is_none(), "{v}");
}

#[tokio::test]
async fn creates_answer_201_with_a_location() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    st.base_url = "/base/".into();
    let app = build_router(st);

    let (s, h, body) = send(&app, api_req("POST", "/api/kernels", Some(json!({})))).await;
    assert_eq!(s, StatusCode::CREATED);
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(h[header::LOCATION], format!("/base/api/kernels/{id}"));

    let session =
        json!({"path": "a b.ipynb", "type": "notebook", "name": "", "kernel": {"name": "fake"}});
    let (s, h, body) = send(&app, api_req("POST", "/api/sessions", Some(session))).await;
    assert_eq!(s, StatusCode::CREATED);
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(h[header::LOCATION], format!("/base/api/sessions/{id}"));

    let file = json!({"type": "file", "format": "text", "content": "x"});
    let (s, h, _) = send(
        &app,
        api_req("PUT", "/api/contents/a%20b.txt", Some(file.clone())),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(h[header::LOCATION], "/base/api/contents/a%20b.txt");
    let (s, h, _) = send(&app, api_req("PUT", "/api/contents/a%20b.txt", Some(file))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.get(header::LOCATION).is_none());

    let (s, h, _) = send(
        &app,
        api_req("POST", "/api/contents", Some(json!({"type": "directory"}))),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(h[header::LOCATION], "/base/api/contents/Untitled%20Folder");

    let (s, h, _) = send(
        &app,
        api_req("POST", "/api/contents/a%20b.txt/checkpoints", None),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        h[header::LOCATION],
        "/base/api/contents/a%20b.txt/checkpoints/checkpoint"
    );
}

#[tokio::test]
async fn allowed_origins_get_cors_headers_and_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    st.allow_origins = vec!["https://app.example".into()];
    let app = build_router(st);

    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/api/kernels")
            .header(header::HOST, HOST)
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap()
    };
    let (s, h, _) = send(&app, preflight("https://app.example")).await;
    assert!(s.is_success(), "{s}");
    assert_eq!(
        h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example"
    );
    assert!(h[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap()
        .contains("POST"));
    assert!(h[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap()
        .contains("authorization"));

    let (_, h, _) = send(&app, preflight("https://evil.example")).await;
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());

    let mut req = api_req("GET", "/api/status", None);
    req.headers_mut()
        .insert(header::ORIGIN, "https://app.example".parse().unwrap());
    let (s, h, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example"
    );

    // Credentials are off by default, as in jupyter_server, so a browser
    // does not send the login cookie cross-origin.
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).is_none());

    // Without allow_origins there are no CORS headers.
    let app = build_router(state(dir.path()));
    let (_, h, _) = send(&app, preflight("https://app.example")).await;
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
}

#[tokio::test]
async fn allow_credentials_lets_listed_origins_send_the_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let app_with = |origins: &[&str]| {
        let mut st = state(dir.path());
        st.allow_origins = origins.iter().map(|o| o.to_string()).collect();
        st.allow_credentials = true;
        build_router(st)
    };
    let request = |origin: &str| {
        let mut req = api_req("GET", "/api/status", None);
        req.headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        req
    };

    let app = app_with(&["https://app.example"]);
    let (_, h, _) = send(&app, request("https://app.example")).await;
    assert_eq!(h[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
    let (_, h, _) = send(&app, request("https://evil.example")).await;
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_none());

    // With `*` every origin would get the cookie, so credentials stay off.
    let app = app_with(&["*"]);
    let (_, h, _) = send(&app, request("https://evil.example")).await;
    assert_eq!(
        h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://evil.example"
    );
    assert!(h.get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS).is_none());
}

#[tokio::test]
async fn login_cookie_is_secure_behind_a_trusted_https_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let cookie_for = |trust: bool, proto: Option<&'static str>| {
        let mut st = state(dir.path());
        st.trust_xheaders = trust;
        let app = build_router(st);
        async move {
            let mut req = api_req("GET", "/api/status", None);
            if let Some(p) = proto {
                req.headers_mut()
                    .insert("x-forwarded-proto", p.parse().unwrap());
            }
            let (_, h, _) = send(&app, req).await;
            set_cookies(&h)[0].2.clone()
        }
    };
    assert!(cookie_for(true, Some("https")).await.ends_with("; Secure"));
    assert!(cookie_for(true, Some("http, https"))
        .await
        .ends_with("; Secure"));
    assert!(!cookie_for(true, Some("http")).await.contains("Secure"));
    assert!(!cookie_for(true, None).await.contains("Secure"));
    assert!(!cookie_for(false, Some("https")).await.contains("Secure"));
}

#[tokio::test]
async fn me_answers_requested_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);
    let (s, me) = call(&app, "GET", "/api/me", None, t).await;
    assert_eq!((s, &me["permissions"]), (StatusCode::OK, &json!({})));
    let q = "%7B%22contents%22%3A%5B%22read%22%2C%22write%22%5D%7D";
    let (s, me) = call(&app, "GET", &format!("/api/me?permissions={q}"), None, t).await;
    assert_eq!(
        (s, &me["permissions"]),
        (StatusCode::OK, &json!({"contents": ["read", "write"]}))
    );
    let (s, _) = call(&app, "GET", "/api/me?permissions=%5B1%5D", None, t).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[cfg(unix)]
#[tokio::test]
async fn errors_name_api_paths_not_os_paths() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "s").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let app = build_router(state(dir.path()));
    let (s, body) = call(
        &app,
        "GET",
        "/api/contents/link/secret.txt",
        None,
        Some(TOKEN),
    )
    .await;
    // 404 like jupyter_server's "is outside root contents directory".
    assert_eq!(s, StatusCode::NOT_FOUND);
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("link/secret.txt"), "{message}");
    assert!(
        !message.contains(&*dir.path().to_string_lossy()),
        "{message}"
    );
}

#[tokio::test]
async fn login_next_with_control_characters_stays_on_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = lab_state(dir.path());
    // With auth off, /login redirects at once, as it does for a browser
    // that is already logged in.
    st.token = None;
    let app = build_router(st);
    for next in [
        "%2F%09%2Fevil.example",
        "%2F%0A%2Fevil.example",
        "%2F%0D%2Fevil.example",
        "%09%2F%2Fevil.example",
        "%2F%20%2Fevil.example",
        "%2F%2Fevil.example",
        "%2F%5Cevil.example",
        "https%3A%2F%2Fevil.example",
    ] {
        let (st, h, _) = send(&app, get_req(&format!("/login?next={next}"), None)).await;
        assert_eq!(st, StatusCode::FOUND, "{next}");
        assert_eq!(h[header::LOCATION], "/", "{next}");
    }
    let (_, h, _) = send(&app, get_req("/login?next=%2Flab%2Ftree%2Fa.ipynb", None)).await;
    assert_eq!(h[header::LOCATION], "/lab/tree/a.ipynb");
}

/// A request with extra headers; returns status, headers and JSON body.
async fn call_with(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("token {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        req = req.header(name, *value);
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

#[tokio::test]
async fn contents_etag_and_if_match() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let text = |s: &str| Some(json!({"type": "file", "format": "text", "content": s}));
    let etag = |h: &axum::http::HeaderMap| h[header::ETAG].to_str().unwrap().to_string();

    // If-Match on a missing file fails and creates nothing.
    let (st, _, _) = call_with(
        &app,
        "PUT",
        "/api/contents/a.txt",
        text("v1"),
        &[(header::IF_MATCH, "*")],
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED);
    assert!(!dir.path().join("a.txt").exists());

    // Saves and content GETs carry the sha256 as ETag; the body keeps
    // jupyter_server's null hash unless ?hash=1.
    let (st, h, saved) = call_with(&app, "PUT", "/api/contents/a.txt", text("v1"), &[]).await;
    assert_eq!(st, StatusCode::CREATED);
    let sha_v1 = "\"3bfc269594ef649228e9a74bab00f042efc91d5acc6fbee31a382e80d42388fe\"";
    assert_eq!(etag(&h), sha_v1);
    assert!(saved["hash"].is_null() && saved["hash_algorithm"].is_null());
    let (_, h, got) = call_with(&app, "GET", "/api/contents/a.txt", None, &[]).await;
    assert_eq!(etag(&h), sha_v1);
    assert!(got["hash"].is_null());
    let (_, h, _) = call_with(&app, "GET", "/api/contents/a.txt?content=0", None, &[]).await;
    assert!(h.get(header::ETAG).is_none());
    let (_, _, got) = call_with(&app, "GET", "/api/contents/a.txt?hash=1", None, &[]).await;
    assert_eq!(format!("\"{}\"", got["hash"].as_str().unwrap()), sha_v1);

    // A matching tag (in a list, beside a weak one) saves.
    let (st, h, _) = call_with(
        &app,
        "PUT",
        "/api/contents/a.txt",
        text("v2"),
        &[(header::IF_MATCH, &format!("W/\"x\", {sha_v1}"))],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tag_v2 = etag(&h);

    // Someone else writes; the old tag no longer matches.
    std::fs::write(dir.path().join("a.txt"), "external").unwrap();
    for stale in [tag_v2.clone(), format!("W/{tag_v2}")] {
        let (st, _, err) = call_with(
            &app,
            "PUT",
            "/api/contents/a.txt",
            text("v3"),
            &[(header::IF_MATCH, &stale)],
        )
        .await;
        assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "external"
    );
}

/// `/api/collaboration`: a y-websocket client opens a notebook room,
/// edits it, sees the edit saved, saves on request, and sees a change made
/// to the file on disk.
#[tokio::test]
async fn collaboration_room_syncs_saves_and_reloads() {
    use ganymede_documents::schema::{self, Roots};
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Text, Transact, Update};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("demo.ipynb");
    let original = "{\n \"cells\": [\n  {\n   \"cell_type\": \"markdown\",\n   \"id\": \"a\",\n   \"metadata\": {},\n   \"source\": [\n    \"hello\"\n   ]\n  }\n ],\n \"metadata\": {\n  \"big\": 18446744073709551616\n },\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n";
    std::fs::write(&file, original).unwrap();
    let app = build_router(state(dir.path()));

    let body = json!({"format": "json", "type": "notebook"});
    let (s, v) = call(
        &app,
        "PUT",
        "/api/collaboration/session/demo.ipynb",
        Some(body.clone()),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, again) = call(
        &app,
        "PUT",
        "/api/collaboration/session/demo.ipynb",
        Some(body.clone()),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["fileId"], again["fileId"]);
    assert_eq!(v["type"], "notebook");
    let (s, _) = call(
        &app,
        "PUT",
        "/api/collaboration/session/nope.ipynb",
        Some(body),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = |session: &str| {
        let mut req = format!(
            "ws://{addr}/api/collaboration/room/json:notebook:{}?sessionId={session}",
            v["fileId"].as_str().unwrap()
        )
        .into_client_request()
        .unwrap();
        req.headers_mut()
            .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
        req
    };

    // A stale session is told to reload.
    let (mut stale, _) = tokio_tungstenite::connect_async(url("old")).await.unwrap();
    match stale.next().await {
        Some(Ok(TMsg::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 1003),
        other => panic!("expected a close, got {other:?}"),
    }

    let (mut ws, _) = tokio_tungstenite::connect_async(url(v["sessionId"].as_str().unwrap()))
        .await
        .unwrap();
    let doc = Doc::new();
    let roots = Roots::get(&doc);
    let sv = doc.transact().state_vector();
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::SyncStep1(sv)).encode_v1().into(),
    ))
    .await
    .unwrap();

    // Apply frames until `until` holds, answering the server's step 1.
    async fn pump<S>(ws: &mut S, doc: &Doc, until: impl Fn(&Doc) -> bool) -> Vec<Vec<u8>>
    where
        S: futures::Stream<Item = Result<TMsg, tokio_tungstenite::tungstenite::Error>>
            + futures::Sink<TMsg>
            + Unpin,
        <S as futures::Sink<TMsg>>::Error: std::fmt::Debug,
    {
        let mut raw = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !until(doc) {
            let msg = tokio::time::timeout_at(deadline, ws.next())
                .await
                .expect("timed out waiting for the room")
                .unwrap()
                .unwrap();
            let TMsg::Binary(b) = msg else { continue };
            if b.first() == Some(&2) {
                raw.push(b.to_vec());
                continue;
            }
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
        raw
    }
    let source = |doc: &Doc| -> Option<String> {
        let roots = Roots::get(doc);
        let txn = doc.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            return None;
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            return None;
        };
        Some(ganymede_documents::yrs::GetString::get_string(&t, &txn))
    };
    pump(&mut ws, &doc, |d| source(d).is_some()).await;
    assert_eq!(source(&doc).as_deref(), Some("hello"));

    // Edit; the room saves a second later, keeping what it did not touch.
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.insert(&mut txn, 5, ", world");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&file)
        .unwrap()
        .contains("hello, world")
    {
        assert!(std::time::Instant::now() < deadline, "edit was not saved");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original.replace("\"hello\"", "\"hello, world\"")
    );

    // An explicit save is answered.
    ws.send(TMsg::Binary(vec![2, 4, b's', b'a', b'v', b'e', 9].into()))
        .await
        .unwrap();
    let raw = loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let TMsg::Binary(b) = msg {
            if b.first() == Some(&2) {
                break b.to_vec();
            }
        }
    };
    let reply: Value = serde_json::from_slice(&raw[2..]).unwrap();
    assert_eq!(reply["responseTo"], 9);
    assert_eq!(reply["status"], "success");

    // A change on disk reaches the client.
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&file, original.replace("\"hello\"", "\"changed on disk\"")).unwrap();
    pump(&mut ws, &doc, |d| {
        source(d).as_deref() == Some("changed on disk")
    })
    .await;
    let roots = Roots::get(&doc);
    let nb = schema::get(&doc.transact(), &roots).unwrap();
    assert_eq!(nb.cells.len(), 1);

    // An edit in the room and a change on disk to another cell, at the
    // same time: the room merges them and saves both.
    let two_cells = |a: &str, b: &str| {
        format!(
            "{{\n \"cells\": [\n  {{\n   \"cell_type\": \"markdown\",\n   \"id\": \"a\",\n   \"metadata\": {{}},\n   \"source\": [\n    \"{a}\"\n   ]\n  }},\n  {{\n   \"cell_type\": \"markdown\",\n   \"id\": \"b\",\n   \"metadata\": {{}},\n   \"source\": [\n    \"{b}\"\n   ]\n  }}\n ],\n \"metadata\": {{\n  \"big\": 18446744073709551616\n }},\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}}\n"
        )
    };
    std::fs::write(&file, two_cells("changed on disk", "second")).unwrap();
    let cells = |d: &Doc| Roots::get(d).cells.len(&d.transact());
    pump(&mut ws, &doc, |d| cells(d) == 2).await;
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.insert(&mut txn, 0, "room: ");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();
    std::fs::write(&file, two_cells("changed on disk", "second, on disk")).unwrap();
    let merged = two_cells("room: changed on disk", "second, on disk");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(&file).unwrap() != merged {
        assert!(
            std::time::Instant::now() < deadline,
            "not merged: {}",
            std::fs::read_to_string(&file).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A client's save whose merge lets the disk win the cell it edited
    // saves the merge and answers `failed` (#279).
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.insert(&mut txn, 0, "lost: ");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();
    // Written before the save, inside the debounce: the save meets it.
    std::fs::write(&file, two_cells("disk wins", "second, on disk")).unwrap();
    ws.send(TMsg::Binary(vec![2, 4, b's', b'a', b'v', b'e', 10].into()))
        .await
        .unwrap();
    let mut reply = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while reply.is_none() || source(&doc).as_deref() != Some("disk wins") {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("timed out waiting for the save")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = msg else { continue };
        if b.first() == Some(&2) {
            reply = Some(serde_json::from_slice::<Value>(&b[2..]).unwrap());
            continue;
        }
        if let Ok(YMsg::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u))) =
            YMsg::decode_v1(&b)
        {
            doc.transact_mut()
                .apply_update(Update::decode_v1(&u).unwrap())
                .unwrap();
        }
    }
    let reply = reply.unwrap();
    assert_eq!(reply["responseTo"], 10);
    assert_eq!(reply["status"], "failed", "{reply}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        two_cells("disk wins", "second, on disk")
    );

    // A rename through the contents API keeps the file id, and the room
    // follows the file.
    let (s, _) = call(
        &app2,
        "PATCH",
        "/api/contents/demo.ipynb",
        Some(json!({"path": "moved.ipynb"})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, moved) = call(
        &app2,
        "PUT",
        "/api/collaboration/session/moved.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(moved["fileId"], v["fileId"]);
    let path = |d: &Doc| {
        let roots = Roots::get(d);
        let txn = d.transact();
        roots.state.get(&txn, "path").map(|o| o.to_string(&txn))
    };
    pump(&mut ws, &doc, |d| path(d).as_deref() == Some("moved.ipynb")).await;
    let moved_file = dir.path().join("moved.ipynb");
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 1) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.push(&mut txn, "!");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&moved_file)
        .unwrap()
        .contains("second, on disk!")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "not saved after the rename"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!file.exists());

    // A delete through the contents API closes the room's clients with
    // 4404, as a reconnect would find (#279).
    let (s, _) = call(
        &app2,
        "DELETE",
        "/api/contents/moved.ipynb",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("the room closes after a delete");
        match msg {
            Some(Ok(TMsg::Close(Some(frame)))) => {
                assert_eq!(u16::from(frame.code), 4404);
                break;
            }
            Some(Ok(_)) => continue,
            other => panic!("expected a close, got {other:?}"),
        }
    }
    let (mut again, _) = tokio_tungstenite::connect_async(url(v["sessionId"].as_str().unwrap()))
        .await
        .unwrap();
    match again.next().await {
        Some(Ok(TMsg::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 4404),
        other => panic!("expected a close, got {other:?}"),
    }
    assert!(!moved_file.exists(), "the room doesn't save the file back");
}

/// A file rewritten more often than the debounce still reaches the room,
/// once the first unsettled event is 5 s old (#279).
#[tokio::test]
async fn collaboration_room_reloads_a_file_that_never_settles() {
    // Events never settle for the 500 ms debounce.
    room_keeps_up_with_a_busy_file(Duration::from_millis(100), Duration::from_secs(8)).await;
}

/// A file rewritten every 0.7 s: each change settles and reloads, and
/// the room's own edit still gets saved in between (#347).
#[tokio::test]
async fn collaboration_room_saves_between_settled_reloads() {
    room_keeps_up_with_a_busy_file(Duration::from_millis(700), Duration::from_secs(6)).await;
}

/// Contents that note whether a save wrote the room's edit (`room: `),
/// and fail the first `fail_room_saves` such saves.
struct SaveLog {
    inner: Arc<dyn ganymede_contents::Contents>,
    room_saved: Arc<std::sync::atomic::AtomicBool>,
    fail_room_saves: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl ganymede_contents::Contents for SaveLog {
    async fn get(
        &self,
        path: &str,
        options: ganymede_contents::GetOptions,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        self.inner.get(path, options).await
    }
    async fn save(
        &self,
        path: &str,
        request: ganymede_contents::SaveRequest,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        let room = request
            .content
            .as_ref()
            .is_some_and(|c| c.to_string().contains("room: "));
        use std::sync::atomic::Ordering::SeqCst;
        // The room saves one at a time, so the check and the decrement
        // can't race.
        if room && self.fail_room_saves.load(SeqCst) > 0 {
            self.fail_room_saves.fetch_sub(1, SeqCst);
            return Err(std::io::Error::other("disk full").into());
        }
        let r = self.inner.save(path, request).await;
        if room && r.is_ok() {
            self.room_saved
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        r
    }
    async fn delete(&self, path: &str) -> ganymede_contents::Result<()> {
        self.inner.delete(path).await
    }
    async fn rename(
        &self,
        old: &str,
        new: &str,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        self.inner.rename(old, new).await
    }
    async fn new_untitled(
        &self,
        dir: &str,
        kind: ganymede_contents::ContentType,
        ext: Option<&str>,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        self.inner.new_untitled(dir, kind, ext).await
    }
    async fn copy(
        &self,
        from: &str,
        to: Option<&str>,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        self.inner.copy(from, to).await
    }
    async fn list_checkpoints(
        &self,
        path: &str,
    ) -> ganymede_contents::Result<Vec<ganymede_contents::Checkpoint>> {
        self.inner.list_checkpoints(path).await
    }
    async fn create_checkpoint(
        &self,
        path: &str,
    ) -> ganymede_contents::Result<ganymede_contents::Checkpoint> {
        self.inner.create_checkpoint(path).await
    }
    async fn restore_checkpoint(&self, path: &str, id: &str) -> ganymede_contents::Result<()> {
        self.inner.restore_checkpoint(path, id).await
    }
    async fn delete_checkpoint(&self, path: &str, id: &str) -> ganymede_contents::Result<()> {
        self.inner.delete_checkpoint(path, id).await
    }
}

/// Another program rewrites cell `a` every `every` for `writing`; the
/// room must reload while it does, and its own edit to cell `b` must
/// reach the file before the writes stop.
async fn room_keeps_up_with_a_busy_file(every: Duration, writing: Duration) {
    use ganymede_documents::schema::Roots;
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Text, Transact, Update};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("busy.ipynb");
    let cell = |id: &str, text: &str| json!({"cell_type": "markdown", "id": id, "metadata": {}, "source": text});
    let nb = json!({"cells": [cell("a", "start"), cell("b", "keep")],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(&file, nb.to_string()).unwrap();
    let mut st = state(dir.path());
    let room_saved = Arc::new(std::sync::atomic::AtomicBool::new(false));
    st.contents = Arc::new(SaveLog {
        inner: st.contents.clone(),
        room_saved: room_saved.clone(),
        fail_room_saves: Default::default(),
    });
    let app = build_router(st);
    let (s, v) = call(
        &app,
        "PUT",
        "/api/collaboration/session/busy.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut req = format!(
        "ws://{addr}/api/collaboration/room/json:notebook:{}?sessionId={}",
        v["fileId"].as_str().unwrap(),
        v["sessionId"].as_str().unwrap()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let doc = Doc::new();
    let roots = Roots::get(&doc);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::SyncStep1(doc.transact().state_vector()))
            .encode_v1()
            .into(),
    ))
    .await
    .unwrap();
    // Take in the room's notebook.
    while roots.cells.len(&doc.transact()) < 2 {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for the room")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = msg else { continue };
        if let Ok(YMsg::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u))) =
            YMsg::decode_v1(&b)
        {
            doc.transact_mut()
                .apply_update(Update::decode_v1(&u).unwrap())
                .unwrap();
        }
    }

    // The writer rewrites cell `a`, keeping the rest of the file. Its
    // read-modify-write can overwrite a room save that lands in between,
    // as any such program would, so the room's saves are counted where
    // it makes them, not read back from the file.
    let writer = {
        let file = file.clone();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut i = 0;
            while start.elapsed() < writing {
                i += 1;
                let mut nb: Value =
                    serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
                nb["cells"][0]["source"] = json!(format!("write {i}"));
                // Whole files only: a reload never reads a half-written one.
                let tmp = file.with_extension("tmp");
                std::fs::write(&tmp, nb.to_string()).unwrap();
                std::fs::rename(&tmp, &file).unwrap();
                std::thread::sleep(every);
            }
        })
    };
    let start = std::time::Instant::now();
    // Edit cell `b` while the file is busy.
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 1) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.insert(&mut txn, 0, "room: ");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();

    let deadline = tokio::time::Instant::now() + writing + Duration::from_secs(5);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("timed out waiting for a reload")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = msg else { continue };
        if let Ok(YMsg::Sync(SyncMessage::Update(_))) = YMsg::decode_v1(&b) {
            break;
        }
    }
    assert!(
        start.elapsed() < writing,
        "the room waited for the writes to stop ({:?})",
        start.elapsed()
    );
    // Wait without blocking the runtime the room runs on.
    while !writer.is_finished() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    writer.join().unwrap();
    assert!(
        room_saved.load(std::sync::atomic::Ordering::SeqCst),
        "the room's edit wasn't saved while the file kept changing"
    );
}

/// A room on `fail.ipynb` (cell `a`, "keep") whose first `fail` saves of
/// its own edit fail, with one client that has typed "room: " into the
/// cell. Returns the client's socket with the file and the count of
/// failures left.
async fn room_edit_with_failing_saves(
    fail: usize,
    cleanup_delay: Option<Duration>,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) {
    use ganymede_documents::schema::Roots;
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Text, Transact, Update};
    use std::sync::atomic::AtomicUsize;
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fail.ipynb");
    let nb = json!({"cells": [{"cell_type": "markdown", "id": "a", "metadata": {}, "source": "keep"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(&file, nb.to_string()).unwrap();
    let mut st = state(dir.path());
    if let Some(delay) = cleanup_delay {
        st.collab = Arc::new(
            ganymede_server::collab::Collab::new(
                ganymede_contents::FileIds::in_memory(),
                dir.path().canonicalize().ok(),
            )
            .with_cleanup_delay(delay),
        );
    }
    let fail = Arc::new(AtomicUsize::new(fail));
    st.contents = Arc::new(SaveLog {
        inner: st.contents.clone(),
        room_saved: Default::default(),
        fail_room_saves: fail.clone(),
    });
    let app = build_router(st);
    let (s, v) = call(
        &app,
        "PUT",
        "/api/collaboration/session/fail.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut req = format!(
        "ws://{addr}/api/collaboration/room/json:notebook:{}?sessionId={}",
        v["fileId"].as_str().unwrap(),
        v["sessionId"].as_str().unwrap()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let doc = Doc::new();
    let roots = Roots::get(&doc);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::SyncStep1(doc.transact().state_vector()))
            .encode_v1()
            .into(),
    ))
    .await
    .unwrap();
    while roots.cells.len(&doc.transact()) < 1 {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for the room")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = msg else { continue };
        if let Ok(YMsg::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u))) =
            YMsg::decode_v1(&b)
        {
            doc.transact_mut()
                .apply_update(Update::decode_v1(&u).unwrap())
                .unwrap();
        }
    }

    // One edit; its first autosave fails.
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            panic!()
        };
        let Some(Out::YText(t)) = cell.get(&txn, "source") else {
            panic!()
        };
        t.insert(&mut txn, 0, "room: ");
    }
    let u = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(u)).encode_v1().into(),
    ))
    .await
    .unwrap();

    (dir, file, fail, ws)
}

/// Wait for the room's edit to reach `file`.
async fn wait_for_room_edit(file: &std::path::Path, fail: &std::sync::atomic::AtomicUsize) {
    use std::sync::atomic::Ordering::SeqCst;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !std::fs::read_to_string(file)
        .unwrap()
        .contains("room: keep")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the room didn't retry its failed save (failures left: {})",
            fail.load(SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(fail.load(SeqCst), 0, "the saves didn't fail");
}

/// A room whose save fails tries again without a further edit (#353).
#[tokio::test]
async fn collaboration_room_retries_a_failed_save() {
    let (_dir, file, fail, ws) = room_edit_with_failing_saves(1, None).await;
    wait_for_room_edit(&file, &fail).await;
    drop(ws);
}

/// A room whose clients left keeps its unsaved edits while its saves
/// fail, instead of retiring with them (#359).
#[tokio::test]
async fn collaboration_room_outlives_cleanup_while_its_save_fails() {
    let (_dir, file, fail, ws) =
        room_edit_with_failing_saves(2, Some(Duration::from_millis(100))).await;
    drop(ws);
    wait_for_room_edit(&file, &fail).await;
}

#[tokio::test]
async fn terminals_are_404_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    for (method, path) in [
        ("GET", "/api/terminals"),
        ("POST", "/api/terminals"),
        ("GET", "/api/terminals/1"),
        ("DELETE", "/api/terminals/1"),
    ] {
        let (s, _) = call(&app, method, path, None, Some(TOKEN)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn terminals_rest_and_websocket() {
    use ganymede_terminals::{TerminalConfig, TerminalManager};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut st = state(dir.path());
    st.terminals = Some(Arc::new(TerminalManager::new(TerminalConfig {
        shell: vec!["/bin/sh".into()],
        root_dir: dir.path().into(),
    })));
    let app = build_router(st);

    assert_eq!(
        call(&app, "GET", "/api/terminals", None, None).await.0,
        StatusCode::FORBIDDEN
    );
    let (s, list) = call(&app, "GET", "/api/terminals", None, Some(TOKEN)).await;
    assert_eq!((s, list), (StatusCode::OK, json!([])));
    let (s, t) = call(&app, "POST", "/api/terminals", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["name"], "1");
    assert!(t["last_activity"].as_str().unwrap().ends_with('Z'), "{t}");
    let body = json!({"name": "work", "cwd": "sub"});
    let (s, t) = call(
        &app,
        "POST",
        "/api/terminals",
        Some(body.clone()),
        Some(TOKEN),
    )
    .await;
    assert_eq!((s, &t["name"]), (StatusCode::OK, &json!("work")));
    let (s, _) = call(&app, "POST", "/api/terminals", Some(body), Some(TOKEN)).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, list) = call(&app, "GET", "/api/terminals", None, Some(TOKEN)).await;
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].clone())
        .collect();
    assert_eq!(names, [json!("1"), json!("work")]);
    let (s, t) = call(&app, "GET", "/api/terminals/work", None, Some(TOKEN)).await;
    assert_eq!((s, &t["name"]), (StatusCode::OK, &json!("work")));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    let connect = |name: &str, token: Option<&str>| {
        let mut req = format!("ws://{addr}/terminals/websocket/{name}")
            .into_client_request()
            .unwrap();
        if let Some(t) = token {
            req.headers_mut()
                .insert("Authorization", format!("token {t}").parse().unwrap());
        }
        tokio_tungstenite::connect_async(req)
    };
    assert!(connect("work", None).await.is_err());
    assert!(connect("missing", Some(TOKEN)).await.is_err());
    let (mut ws, _) = connect("work", Some(TOKEN)).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("setup in time")
        .unwrap()
        .unwrap();
    assert_eq!(first, TMsg::Text(json!(["setup", {}]).to_string().into()));
    for msg in [
        json!(["set_size", 33, 101, 600, 800]),
        json!(["stdin", "stty size; pwd; echo done-$((20+22))\n"]),
    ] {
        ws.send(TMsg::Text(msg.to_string().into())).await.unwrap();
    }
    let mut out = String::new();
    while !out.contains("done-42") {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("output in time")
            .unwrap()
            .unwrap();
        let TMsg::Text(t) = frame else { continue };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        assert_eq!(v[0], "stdout", "{v}");
        out.push_str(v[1].as_str().unwrap());
    }
    assert!(out.contains("33 101"), "{out}");
    assert!(out.contains("/sub"), "{out}");

    let (s, _) = call(&app, "DELETE", "/api/terminals/work", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let mut disconnected = false;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        if let TMsg::Text(t) = frame {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v == json!(["disconnect", 1]) {
                disconnected = true;
                break;
            }
        }
    }
    assert!(disconnected);
    let (s, _) = call(&app, "GET", "/api/terminals/work", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&app, "DELETE", "/api/terminals/work", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lab_page_config_reports_terminals() {
    use ganymede_terminals::{TerminalConfig, TerminalManager};
    let dir = tempfile::tempdir().unwrap();
    let mut st = lab_state(dir.path());
    st.terminals = Some(Arc::new(TerminalManager::new(TerminalConfig::default())));
    let app = build_router(st);
    let (s, html) = get_text_with(
        &app,
        "/lab",
        &[(header::AUTHORIZATION, &format!("token {TOKEN}"))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("\"terminalsAvailable\":true"), "{html}");
}

#[tokio::test]
async fn files_serves_raw_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(
        dir.path().join("sub/my pic.png"),
        [0x89, b'P', b'N', b'G', 0xff],
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
    std::fs::write(dir.path().join("page.html"), "<script>1</script>").unwrap();
    std::fs::write(dir.path().join("a.ipynb"), "{\"cells\": []}").unwrap();
    std::fs::write(dir.path().join("blob"), [0xff, 0xfe]).unwrap();
    std::fs::write(dir.path().join(".secret"), "no").unwrap();
    let app = build_router(state(dir.path()));
    let get = |path: &str| {
        Request::get(path)
            .header(header::AUTHORIZATION, format!("token {TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };

    let (st, h, body) = raw(&app, get("/files/sub/my%20pic.png")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert!(h.get(header::CONTENT_DISPOSITION).is_none());
    assert_eq!(body, [0x89, b'P', b'N', b'G', 0xff]);

    let (_, h, _) = raw(&app, get("/files/sub/my%20pic.png?download=1")).await;
    assert_eq!(
        h[header::CONTENT_DISPOSITION],
        "attachment; filename*=utf-8''my%20pic.png"
    );

    // HTML runs sandboxed in an opaque origin, as jupyter_server's.
    let (st, h, _) = raw(&app, get("/files/page.html")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], "sandbox allow-scripts");

    let (st, h, body) = raw(&app, get("/files/notes.txt")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "text/plain; charset=UTF-8");
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], "sandbox allow-scripts");
    assert_eq!(body, b"hello");

    let (st, h, body) = raw(&app, get("/files/a.ipynb")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/x-ipynb+json");
    assert_eq!(body, b"{\"cells\": []}");

    let (_, h, body) = raw(&app, get("/files/blob")).await;
    assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(body, [0xff, 0xfe]);

    // As jupyter_server: hidden and missing files and escapes are 404,
    // a directory is 400, and the token is required.
    for path in [
        "/files/.secret",
        "/files/missing.txt",
        "/files/..%2F..%2Fetc%2Fpasswd",
    ] {
        assert_eq!(
            raw(&app, get(path)).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        raw(&app, get("/files/sub")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "GET", "/files/notes.txt", None, None).await.0,
        StatusCode::FORBIDDEN
    );
}

async fn raw(app: &axum::Router, req: Request<Body>) -> (StatusCode, header::HeaderMap, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

#[tokio::test]
async fn language_servers_are_404_when_none_configured() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let (s, _) = call(&app, "GET", "/lsp/status", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// `cat` as the language server: it echoes each framed message, so a
/// client's message comes back through the server's stdout.
#[cfg(unix)]
#[tokio::test]
async fn language_server_status_and_websocket() {
    use ganymede_lsp::{LanguageServerSpec, LspConfig, LspManager};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    let spec: LanguageServerSpec = serde_json::from_value(json!({
        "argv": ["cat"],
        "languages": ["python"],
        "mime_types": ["text/x-python"],
        "env": {"SECRET": "x"},
        "display_name": "Echo",
    }))
    .unwrap();
    st.lsp = Some(Arc::new(LspManager::new(LspConfig {
        servers: [("echo".to_string(), spec)].into(),
        root_dir: dir.path().into(),
    })));
    let app = build_router(st);

    assert_eq!(
        call(&app, "GET", "/lsp/status", None, None).await.0,
        StatusCode::FORBIDDEN
    );
    let (s, status) = call(&app, "GET", "/lsp/status", None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(status["version"], 2);
    assert_eq!(status["specs"]["echo"]["argv"], json!([""]));
    assert_eq!(status["specs"]["echo"]["env"], json!({}));
    assert_eq!(status["specs"]["echo"]["display_name"], "Echo");
    assert_eq!(status["sessions"]["echo"]["status"], "not_started");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    let connect = |name: &str, token: Option<&str>| {
        let mut req = format!("ws://{addr}/lsp/ws/{name}")
            .into_client_request()
            .unwrap();
        if let Some(t) = token {
            req.headers_mut()
                .insert("Authorization", format!("token {t}").parse().unwrap());
        }
        tokio_tungstenite::connect_async(req)
    };
    assert!(connect("echo", None).await.is_err());
    assert!(connect("missing", Some(TOKEN)).await.is_err());
    let (mut a, _) = connect("echo", Some(TOKEN)).await.unwrap();
    let (mut b, _) = connect("echo", Some(TOKEN)).await.unwrap();
    let (_, status) = call(&app, "GET", "/lsp/status", None, Some(TOKEN)).await;
    assert_eq!(status["sessions"]["echo"]["status"], "started");
    assert_eq!(status["sessions"]["echo"]["handler_count"], 2);

    // A notification goes to every client (a request would reach one).
    let msg = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string();
    a.send(TMsg::Text(msg.clone().into())).await.unwrap();
    for ws in [&mut a, &mut b] {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("reply in time")
            .unwrap()
            .unwrap();
        assert_eq!(frame, TMsg::Text(msg.clone().into()));
    }

    a.close(None).await.unwrap();
    b.close(None).await.unwrap();
    // The server stops once both sockets are gone.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (_, status) = call(&app, "GET", "/lsp/status", None, Some(TOKEN)).await;
        if status["sessions"]["echo"]["status"] == "stopped" {
            assert_eq!(status["sessions"]["echo"]["handler_count"], 0);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{status}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// jupyter-server-documents' execute endpoint: cells run on the server
/// into the room, with no client connected, and land in the file.
#[tokio::test]
async fn execute_endpoint_runs_cells_into_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("run.ipynb");
    let code = |id: &str, src: &str| json!({"cell_type": "code", "execution_count": null, "id": id, "metadata": {}, "outputs": [], "source": src});
    let nb = json!({
        "cells": [code("a", "hello"), code("b", "display"), code("c", "raise"), code("d", "never"),
                  {"cell_type": "markdown", "id": "m", "metadata": {}, "source": "text"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5
    });
    std::fs::write(&file, serde_json::to_string_pretty(&nb).unwrap()).unwrap();
    let app = build_router(state(dir.path()));
    let (s, kernel) = call(&app, "POST", "/api/kernels", Some(json!({})), Some(TOKEN)).await;
    assert_eq!(s, StatusCode::CREATED);
    let kid = kernel["id"].as_str().unwrap().to_string();
    let (_, session) = call(
        &app,
        "PUT",
        "/api/collaboration/session/run.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    let doc = format!("json:notebook:{}", session["fileId"].as_str().unwrap());
    // jupyter-server-documents' and jupyter_server_fileid's id endpoints
    // agree with the session's.
    let (s, v) = call(
        &app,
        "POST",
        "/api/fileid/index?path=run.ipynb",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"id": session["fileId"], "path": "run.ipynb"}));
    let (s, v) = call(
        &app,
        "GET",
        "/api/fileid/id?path=run.ipynb",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!((s, &v["id"]), (StatusCode::OK, &session["fileId"]));
    let id = session["fileId"].as_str().unwrap();
    let (s, v) = call(
        &app,
        "GET",
        &format!("/api/fileid/path?id={id}"),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!((s, &v["path"]), (StatusCode::OK, &json!("run.ipynb")));
    let (s, _) = call(
        &app,
        "POST",
        "/api/fileid/index?path=missing.ipynb",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &app,
        "GET",
        "/api/fileid/id?path=other.ipynb",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // nb-cli's probe: an empty request is a 400, not a 404.
    let (s, _) = call(
        &app,
        "POST",
        &format!("/api/kernels/{kid}/execute"),
        Some(json!({"document_id": "", "cells": []})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let hash = |src: &str| ganymede_documents::murmur2(src.as_bytes(), 0).to_string();
    let url = format!("/api/kernels/{kid}/execute");
    let exec = |body: Value| call(&app, "POST", &url, Some(body), Some(TOKEN));

    // Refusals.
    let (s, v) = exec(json!({"document_id": doc, "cells": [
        {"cell_id": "a", "source_hash": hash("hello")},
        {"cell_id": "b", "source_hash": hash("changed")}]}))
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v, json!({"error": "source_mismatch", "cell_id": "b"}));
    let (s, _) =
        exec(json!({"document_id": doc, "cells": [{"cell_id": "m", "source_hash": hash("text")}]}))
            .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "markdown cell");
    let (s, _) =
        exec(json!({"document_id": doc, "cells": [{"cell_id": "zz", "source_hash": "0"}]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown cell");
    let (s, _) = exec(json!({"document_id": "json:notebook:nope", "cells": [{"cell_id": "a", "source_hash": "0"}]})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown document");
    let (s, _) = exec(json!({"document_id": doc, "cells": []})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "no cells");
    let (s, _) = call(
        &app,
        "POST",
        "/api/kernels/nope/execute",
        Some(
            json!({"document_id": doc, "cells": [{"cell_id": "a", "source_hash": hash("hello")}]}),
        ),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "unknown kernel");

    // Two chained requests: a, then b, c, d. c fails, so d is skipped.
    let (s, v) = exec(json!({"document_id": doc, "request_id": "r1",
        "cells": [{"cell_id": "a", "source_hash": hash("hello")}]}))
    .await;
    assert_eq!((s, v), (StatusCode::OK, Value::Null));
    let (s, _) = exec(
        json!({"document_id": doc, "request_id": "r2", "previous_request_id": "r1",
        "cells": [
            {"cell_id": "b", "source_hash": hash("display")},
            {"cell_id": "c", "source_hash": hash("raise")},
            {"cell_id": "d", "source_hash": hash("never")}]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let saved = loop {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        if v["cells"][2]["outputs"]
            .as_array()
            .is_some_and(|o| !o.is_empty())
            && v["cells"][2]["execution_count"].is_number()
        {
            break v;
        }
        assert!(std::time::Instant::now() < deadline, "not run: {v:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let cells = &saved["cells"];
    assert_eq!(cells[0]["execution_count"], 1);
    assert!(
        cells[0]["outputs"].to_string().contains("hello"),
        "{:#}",
        cells[0]
    );
    assert_eq!(cells[1]["execution_count"], 2);
    // The display was updated, then cleared by the wait-clear before the
    // stream.
    assert_eq!(
        cells[1]["outputs"].as_array().unwrap().len(),
        1,
        "{:#}",
        cells[1]
    );
    assert_eq!(cells[1]["outputs"][0]["text"], json!(["after\n"]));
    assert_eq!(cells[2]["outputs"][0]["output_type"], "error");
    assert_eq!(cells[3]["execution_count"], Value::Null);
    assert_eq!(cells[3]["outputs"], json!([]));
    assert!(
        cells[0].get("execution_state").is_none(),
        "runtime state is not saved"
    );
}

/// A progress-style cell (clear with wait, then print) updates a
/// connected client live, before the cell finishes.
#[tokio::test]
async fn execute_clear_wait_updates_live() {
    use ganymede_documents::schema::Roots;
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Transact, Update};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let nb = json!({
        "cells": [{"cell_type": "code", "execution_count": null, "id": "p", "metadata": {},
                   "outputs": [], "source": "progress"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5
    });
    std::fs::write(dir.path().join("p.ipynb"), nb.to_string()).unwrap();
    let app = build_router(state(dir.path()));
    let (_, kernel) = call(&app, "POST", "/api/kernels", Some(json!({})), Some(TOKEN)).await;
    let kid = kernel["id"].as_str().unwrap().to_string();
    let (_, session) = call(
        &app,
        "PUT",
        "/api/collaboration/session/p.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    let file_id = session["fileId"].as_str().unwrap().to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    let mut req = format!(
        "ws://{addr}/api/collaboration/room/json:notebook:{file_id}?sessionId={}",
        session["sessionId"].as_str().unwrap()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let doc = Doc::new();
    let sv = doc.transact().state_vector();
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::SyncStep1(sv)).encode_v1().into(),
    ))
    .await
    .unwrap();

    // The cell's outputs as JSON, and whether it is still running.
    let seen = |doc: &Doc| -> (Value, bool) {
        let roots = Roots::get(doc);
        let txn = doc.transact();
        let Some(Out::YMap(cell)) = roots.cells.get(&txn, 0) else {
            return (Value::Null, false);
        };
        let outputs = cell
            .get(&txn, "outputs")
            .map(|o| ganymede_documents::schema::out_to_json(&txn, &o))
            .unwrap_or(Value::Null);
        let running = matches!(
            cell.get(&txn, "execution_state"),
            Some(Out::Any(ganymede_documents::yrs::Any::String(s))) if &*s == "running"
        );
        (outputs, running)
    };
    let mut ran = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (outputs, running) = seen(&doc);
        if running && outputs.as_array().map(Vec::len) == Some(1) && outputs[0]["text"] == "2\n" {
            break;
        }
        if !ran && Roots::get(&doc).cells.len(&doc.transact()) > 0 {
            // Synced: run the cell.
            let hash = ganymede_documents::murmur2(b"progress", 0).to_string();
            let (s, _) = call(
                &app,
                "POST",
                &format!("/api/kernels/{kid}/execute"),
                Some(json!({"document_id": format!("json:notebook:{file_id}"),
                    "cells": [{"cell_id": "p", "source_hash": hash}]})),
                Some(TOKEN),
            )
            .await;
            assert_eq!(s, StatusCode::OK);
            ran = true;
        }
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("no live progress: {:?}", seen(&doc)))
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

/// A cell that prints many lines, one message each, ends with one stream
/// holding all of them, live updates and the final outputs agreeing.
#[tokio::test]
async fn execute_many_stream_messages_into_one_output() {
    const LINES: usize = 5_000;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("many.ipynb");
    let src = format!("lines {LINES}");
    let nb = json!({
        "cells": [{"cell_type": "code", "execution_count": null, "id": "a", "metadata": {},
                   "outputs": [], "source": src}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5
    });
    std::fs::write(&file, serde_json::to_string_pretty(&nb).unwrap()).unwrap();
    let app = build_router(state(dir.path()));
    let (_, kernel) = call(&app, "POST", "/api/kernels", Some(json!({})), Some(TOKEN)).await;
    let kid = kernel["id"].as_str().unwrap().to_string();
    let (_, session) = call(
        &app,
        "PUT",
        "/api/collaboration/session/many.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    let doc = format!("json:notebook:{}", session["fileId"].as_str().unwrap());
    let hash = ganymede_documents::murmur2(src.as_bytes(), 0).to_string();
    let started = std::time::Instant::now();
    let (s, _) = call(
        &app,
        "POST",
        &format!("/api/kernels/{kid}/execute"),
        Some(json!({"document_id": doc, "cells": [{"cell_id": "a", "source_hash": hash}]})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let saved = loop {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        if v["cells"][0]["execution_count"].is_number() {
            break v;
        }
        assert!(std::time::Instant::now() < deadline, "not run");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    eprintln!("{LINES} lines in {:?}", started.elapsed());
    let outputs = saved["cells"][0]["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 1);
    let text: String = outputs[0]["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(text.lines().count(), LINES);
    assert!(text.ends_with(&format!("line {}\n", LINES - 1)));
}

/// A display update reaches the cell that shows the display, and only
/// while it still does: not after the cell ran again.
#[tokio::test]
async fn execute_display_updates_reach_only_current_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("d.ipynb");
    let code = |id: &str, src: &str| json!({"cell_type": "code", "execution_count": null, "id": id, "metadata": {}, "outputs": [], "source": src});
    let write = |a: &str| {
        let nb = json!({"cells": [code("a", a), code("b", "update")],
                        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
        std::fs::write(&file, serde_json::to_string_pretty(&nb).unwrap()).unwrap();
    };
    write("show");
    let app = build_router(state(dir.path()));
    let (_, kernel) = call(&app, "POST", "/api/kernels", Some(json!({})), Some(TOKEN)).await;
    let kid = kernel["id"].as_str().unwrap().to_string();
    let (_, session) = call(
        &app,
        "PUT",
        "/api/collaboration/session/d.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    let doc = format!("json:notebook:{}", session["fileId"].as_str().unwrap());
    let hash = |src: &str| ganymede_documents::murmur2(src.as_bytes(), 0).to_string();
    let url = format!("/api/kernels/{kid}/execute");
    let run = |id: &'static str, src: &'static str| {
        let (app, url, doc) = (app.clone(), url.clone(), doc.clone());
        let body =
            json!({"document_id": doc, "cells": [{"cell_id": id, "source_hash": hash(src)}]});
        async move { call(&app, "POST", &url, Some(body), Some(TOKEN)).await.0 }
    };
    // Waits until cell `i` on disk shows `text` and has a count.
    let saved = |i: usize, text: &'static str| {
        let file = file.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    let v: Value =
                        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
                    let c = &v["cells"][i];
                    if c["outputs"].to_string().contains(text) && c["execution_count"].is_number() {
                        return v;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("cell {i} never showed {text}"))
        }
    };

    assert_eq!(run("a", "show").await, StatusCode::OK);
    saved(0, "shown").await;
    assert_eq!(run("b", "update").await, StatusCode::OK);
    saved(0, "updated").await;

    // a runs again without the display; a later update leaves it alone.
    write("hello");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while run("a", "hello").await == StatusCode::CONFLICT {
        assert!(
            std::time::Instant::now() < deadline,
            "the room never reloaded"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    saved(0, "hello").await;
    assert_eq!(run("b", "update").await, StatusCode::OK);
    // b's own run has finished once its count moves past a's.
    let v = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            if v["cells"][1]["execution_count"].as_i64() > v["cells"][0]["execution_count"].as_i64()
            {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let v2: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for v in [&v, &v2] {
        let a = v["cells"][0]["outputs"].to_string();
        assert!(
            a.contains("hello") && !a.contains("updated") && !a.contains("shown"),
            "{a}"
        );
    }
}

/// One JSON-RPC call to `/mcp`; the response and the session id it set.
async fn mcp_call(
    app: &axum::Router,
    session: Option<&str>,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "localhost:8888")
        .header(header::AUTHORIZATION, format!("token {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18");
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let sid = resp
        .headers()
        .get("mcp-session-id")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    // Server-sent events: the response is the last `data:` message.
    let body = String::from_utf8_lossy(&bytes);
    let v = body
        .lines()
        .rev()
        .filter_map(|l| l.strip_prefix("data: "))
        .find_map(|d| serde_json::from_str::<Value>(d).ok())
        .unwrap_or(Value::String(body.into()));
    (status, sid, v)
}

fn mcp_text(v: &Value) -> String {
    assert!(v.get("error").is_none(), "{v}");
    v["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn mcp_journals_into_a_notebook() {
    let dir = tempfile::tempdir().unwrap();
    let mut st = state(dir.path());
    let app_off = build_router(st.clone());
    let (s, _, _) = mcp_call(&app_off, None, json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "/mcp is off by default");
    st.mcp = Some(Arc::new(ganymede_server::mcp::McpState::new(
        ganymede_server::mcp::McpOptions {
            read_only: false,
            no_execute: false,
            images: false,
            outputs_dir: Some(dir.path().join("spill")),
        },
    )));
    let app = build_router(st);

    // The token is required.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let (s, sid, v) = mcp_call(
        &app,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "test-agent", "version": "1"}}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sid = sid.expect("session id");
    assert!(v["result"]["capabilities"]["tools"].is_object(), "{v}");
    let (s, _, _) = mcp_call(
        &app,
        Some(&sid),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);

    let mut n = 1;
    let mut tool = |name: &'static str, args: Value| {
        n += 1;
        let (app, sid) = (app.clone(), sid.clone());
        async move {
            let (s, _, v) = mcp_call(
                &app,
                Some(&sid),
                json!({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                       "params": {"name": name, "arguments": args}}),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{v}");
            v
        }
    };
    let (_, _, v) = mcp_call(
        &app,
        Some(&sid),
        json!({"jsonrpc": "2.0", "id": 100, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in ["notebook_open", "cell_run", "cell_add", "notebook_read"] {
        assert!(names.contains(&t), "{names:?}");
    }

    let v = tool(
        "notebook_open",
        json!({"path": "journal.ipynb", "title": "Exploring"}),
    )
    .await;
    let out = mcp_text(&v);
    assert!(
        out.starts_with("Created journal.ipynb (server root ") && out.contains(" on kernel fake"),
        "{out}"
    );
    assert!(out.contains("# Exploring"), "{out}");

    let v = tool("cell_add", json!({"source": "Looking at the data."})).await;
    mcp_text(&v);
    let v = tool("cell_run", json!({"source": "hello"})).await;
    assert!(mcp_text(&v).contains("hello"), "{v}");
    // Scratch runs don't store history, so the next recorded cell's
    // count follows the previous one's.
    let v = tool("cell_run", json!({"source": "scratch", "record": false})).await;
    assert!(mcp_text(&v).contains("scratch"), "{v}");
    let v = tool("cell_run", json!({"source": "raise"})).await;
    let out = mcp_text(&v);
    assert!(out.contains("FakeError: raised"), "{out}");
    assert!(!out.contains('\u{1b}'), "ANSI is stripped: {out:?}");
    // A scratch run still works after a recorded cell failed.
    let v = tool("cell_run", json!({"source": "scratch", "record": false})).await;
    assert!(mcp_text(&v).contains("scratch"), "{v}");

    let v = tool("notebook_read", json!({})).await;
    let read = mcp_text(&v);
    assert!(read.contains("Looking at the data."), "{read}");
    assert!(
        !read.contains("scratch"),
        "scratch runs leave nothing: {read}"
    );

    // The room saves to disk: title, note, two recorded runs with
    // provenance and outputs.
    let file = dir.path().join("journal.ipynb");
    let nb = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(s) = std::fs::read_to_string(&file) {
                let nb: Value = serde_json::from_str(&s).unwrap();
                if nb["cells"].as_array().map(Vec::len) == Some(4)
                    && nb["cells"][3]["outputs"].as_array().map(Vec::len) == Some(1)
                    && nb["cells"][3]["execution_count"].is_number()
                {
                    return nb;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("journal saved");
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells[1]["cell_type"], "markdown");
    assert_eq!(cells[2]["source"], json!(["hello"]));
    assert!(cells[2]["outputs"].to_string().contains("hello"));
    assert_eq!(
        cells[2]["metadata"]["ganymede"]["agent"]["by"],
        "test-agent"
    );
    assert_eq!(cells[3]["outputs"][0]["output_type"], "error");
    assert_eq!(cells[2]["execution_count"], 1);
    assert_eq!(cells[3]["execution_count"], 2);
    assert_eq!(nb["metadata"]["kernelspec"]["name"], "fake");
}

/// An initialized MCP session on `app`.
async fn mcp_connect(app: &axum::Router, client: &str) -> String {
    let (s, sid, v) = mcp_call(
        app,
        None,
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": client, "version": "1"}}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sid = sid.expect("session id");
    mcp_call(
        app,
        Some(&sid),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    sid
}

async fn mcp_tool(app: &axum::Router, sid: &str, name: &str, args: Value) -> Value {
    let (s, _, v) = mcp_call(
        app,
        Some(sid),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn mcp_app(dir: &std::path::Path) -> axum::Router {
    let mut st = state(dir);
    st.mcp = Some(Arc::new(ganymede_server::mcp::McpState::new(
        ganymede_server::mcp::McpOptions {
            read_only: false,
            no_execute: false,
            images: false,
            outputs_dir: Some(dir.join("spill")),
        },
    )));
    build_router(st)
}

/// A scratch run that times out interrupts the kernel, so the next run
/// doesn't queue behind it (#323).
#[tokio::test]
async fn mcp_scratch_timeout_interrupts_the_kernel() {
    let dir = tempfile::tempdir().unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "agent").await;
    mcp_text(&mcp_tool(&app, &sid, "notebook_open", json!({"path": "t.ipynb"})).await);

    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "sleep 30000", "record": false, "timeout": 1}),
        )
        .await,
    );
    assert!(out.contains("timed out"), "{out}");
    assert!(out.contains("Interrupted the kernel."), "{out}");
    // Without the interrupt, this would wait out the 30 s sleep.
    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "after", "record": false, "timeout": 10}),
        )
        .await,
    );
    assert!(!out.contains("timed out"), "{out}");
    assert!(out.contains("after"), "{out}");
}

/// A kernel that sends no `execute_input` is running the scratch code once
/// its `busy` arrives, so a timeout still interrupts it (#340).
#[tokio::test]
async fn mcp_scratch_timeout_without_execute_input_interrupts_the_kernel() {
    let dir = tempfile::tempdir().unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "agent").await;
    mcp_text(&mcp_tool(&app, &sid, "notebook_open", json!({"path": "t.ipynb"})).await);

    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "quiet 30000", "record": false, "timeout": 1}),
        )
        .await,
    );
    assert!(out.contains("timed out"), "{out}");
    assert!(out.contains("Interrupted the kernel."), "{out}");
    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "after", "record": false, "timeout": 10}),
        )
        .await,
    );
    assert!(!out.contains("timed out"), "{out}");
    assert!(out.contains("after"), "{out}");
}

/// A scratch run that times out still queued behind another run leaves
/// that run alone: the interrupt would stop it, not the scratch code.
#[tokio::test]
async fn mcp_scratch_timeout_while_queued_interrupts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "agent").await;
    mcp_text(&mcp_tool(&app, &sid, "notebook_open", json!({"path": "q.ipynb"})).await);

    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "sleep 3000", "timeout": 1}),
        )
        .await,
    );
    assert!(out.contains("still running"), "{out}");
    let out = mcp_text(
        &mcp_tool(
            &app,
            &sid,
            "cell_run",
            json!({"source": "queued", "record": false, "timeout": 1}),
        )
        .await,
    );
    assert!(out.contains("hadn't started"), "{out}");
    assert!(!out.contains("Interrupted"), "{out}");
    // The recorded cell finishes as it would have.
    let file = dir.path().join("q.ipynb");
    let nb = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(s) = std::fs::read_to_string(&file) {
                let nb: Value = serde_json::from_str(&s).unwrap();
                if nb["cells"][0]["execution_count"].is_number() {
                    return nb;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the recorded cell finished");
    assert!(
        !nb["cells"][0]["outputs"]
            .to_string()
            .contains("KeyboardInterrupt"),
        "{nb}"
    );
}

/// nbformat 4.4 notebooks keep no cell ids on disk, but the tools address
/// cells by the ids the document gives them.
#[tokio::test]
async fn mcp_works_on_notebooks_without_cell_ids() {
    let dir = tempfile::tempdir().unwrap();
    let nb = json!({"cells": [
        {"cell_type": "markdown", "metadata": {}, "source": "old notes"},
        {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": "first"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 4});
    std::fs::write(dir.path().join("old.ipynb"), nb.to_string()).unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "agent").await;

    mcp_text(&mcp_tool(&app, &sid, "notebook_open", json!({"path": "old.ipynb"})).await);
    let read = mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({})).await);
    assert!(!read.contains("null"), "every cell has an id: {read}");
    let out = mcp_text(&mcp_tool(&app, &sid, "cell_run", json!({"source": "hello"})).await);
    assert!(out.contains("hello"), "the recorded cell ran: {out}");
    let out = mcp_text(&mcp_tool(&app, &sid, "notebook_run_all", json!({"restart": false})).await);
    assert!(out.contains("first"), "{out}");
    // Saved as 4.4: still no ids on disk.
    let file = dir.path().join("old.ipynb");
    let saved = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            if v["cells"].as_array().map(Vec::len) == Some(3)
                && v["cells"][1]["execution_count"].is_number()
            {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("saved");
    assert_eq!(saved["nbformat_minor"], 4);
    assert!(saved["cells"][2].get("id").is_none());
    assert!(saved["cells"][2]["outputs"].to_string().contains("hello"));
}

/// Cell ids a 4.4 notebook's room made up stay valid while the connection
/// lasts, though the room would otherwise retire and reload with new ones.
#[tokio::test]
async fn mcp_cell_ids_outlive_an_idle_room() {
    let dir = tempfile::tempdir().unwrap();
    let nb = json!({"cells": [
        {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": "first"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 4});
    std::fs::write(dir.path().join("old.ipynb"), nb.to_string()).unwrap();
    let mut st = state(dir.path());
    st.collab = Arc::new(
        ganymede_server::collab::Collab::new(
            ganymede_contents::FileIds::in_memory(),
            dir.path().canonicalize().ok(),
        )
        .with_cleanup_delay(Duration::from_millis(100)),
    );
    st.mcp = Some(Arc::new(ganymede_server::mcp::McpState::new(
        ganymede_server::mcp::McpOptions {
            read_only: true,
            no_execute: false,
            images: false,
            outputs_dir: None,
        },
    )));
    let app = build_router(st);
    let sid = mcp_connect(&app, "agent").await;
    let v = mcp_tool(&app, &sid, "notebook_read", json!({"path": "old.ipynb"})).await;
    let read = mcp_text(&v);
    // Several cleanup delays: an unheld room would have retired.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let again =
        mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({"path": "old.ipynb"})).await);
    assert_eq!(read, again, "same ids after the room sat idle");

    // A room that stopped (its file wasn't a notebook yet) is held again
    // once it opens.
    let late = dir.path().join("late.ipynb");
    std::fs::write(&late, "not yet").unwrap();
    let v = mcp_tool(&app, &sid, "notebook_read", json!({"path": "late.ipynb"})).await;
    assert!(
        v.get("error").is_some() || v["result"]["isError"] == true,
        "{v}"
    );
    std::fs::write(&late, nb.to_string()).unwrap();
    let read =
        mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({"path": "late.ipynb"})).await);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let again =
        mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({"path": "late.ipynb"})).await);
    assert_eq!(read, again, "same ids after the room sat idle");
}

/// cell_edit keeps the sources it replaced, bounded, and a no-op edit
/// adds none.
#[tokio::test]
async fn mcp_cell_edit_keeps_superseded_sources() {
    let dir = tempfile::tempdir().unwrap();
    let nb = json!({"cells": [
        {"cell_type": "markdown", "id": "f", "metadata": {}, "source": "v0"},
        {"cell_type": "markdown", "id": "g", "metadata": {}, "source": ""}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(dir.path().join("h.ipynb"), nb.to_string()).unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "editor").await;
    let edit = |src: String| {
        let (app, sid) = (app.clone(), sid.clone());
        async move {
            mcp_text(
                &mcp_tool(
                    &app,
                    &sid,
                    "cell_edit",
                    json!({"path": "h.ipynb", "id": "f", "source": src}),
                )
                .await,
            )
        }
    };
    // Cell g: an empty source replaced records no revision, and an edit
    // that changes nothing records none either.
    edit_by(&app, &sid, "g", "x").await;
    let other = mcp_connect(&app, "other").await;
    edit_by(&app, &other, "g", "x").await;
    // v0 -> v1 ... v12, an edit that changes nothing, then v13. Waiting
    // for v13 on disk means the no-op edit is in the saved file too.
    for n in 1..=12 {
        let out = edit(format!("v{n}")).await;
        assert_eq!(out, "Edited cell f");
    }
    edit("v12".into()).await;
    edit("v13".into()).await;

    let file = dir.path().join("h.ipynb");
    let nb = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(s) = std::fs::read_to_string(&file) {
                let nb: Value = serde_json::from_str(&s).unwrap();
                if nb["cells"][0]["source"] == json!(["v13"]) {
                    return (s, nb);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("edits saved");
    let (raw, nb) = nb;
    let agent = &nb["cells"][0]["metadata"]["ganymede"]["agent"];
    let history = agent["history"].as_array().expect("history");
    // Thirteen replaced sources, the last ten kept; the no-op edit adds
    // none (no second v12).
    let sources: Vec<&str> = history
        .iter()
        .map(|h| h["source"].as_str().unwrap())
        .collect();
    let limit = ganymede_server::mcp::HISTORY_LIMIT;
    let want: Vec<String> = (13 - limit..13).map(|n| format!("v{n}")).collect();
    assert_eq!(sources, want);
    assert_eq!(history.len(), ganymede_server::mcp::HISTORY_LIMIT);
    for h in history {
        assert_eq!(h["by"], "editor", "{h}");
        assert!(h["at"].is_string(), "{h}");
    }
    // A person's cell edited by an agent becomes an agent note.
    assert_eq!(agent["mode"], "note");
    let g = &nb["cells"][1];
    assert_eq!(g["source"], json!(["x"]));
    assert!(
        g["metadata"]["ganymede"]["agent"]["history"].is_null(),
        "{g}"
    );
    // The file still reads as nbformat 4.
    ganymede_nbformat::Notebook::from_str(&raw).expect("valid notebook");
}

async fn edit_by(app: &axum::Router, sid: &str, id: &str, src: &str) {
    let out = mcp_text(
        &mcp_tool(
            app,
            sid,
            "cell_edit",
            json!({"path": "h.ipynb", "id": id, "source": src}),
        )
        .await,
    );
    assert_eq!(out, format!("Edited cell {id}"));
}

/// cell_move checks `after` before it touches the cell, and moves both
/// ways; resource URIs name the same room as the tools' paths.
#[tokio::test]
async fn mcp_cell_move_and_resource_paths() {
    let dir = tempfile::tempdir().unwrap();
    let cells: Vec<Value> = ["a", "b", "c"]
        .iter()
        .map(|s| {
            json!({"cell_type": "code", "id": s, "execution_count": null,
            "metadata": {}, "outputs": [], "source": format!("src_{s}")})
        })
        .collect();
    let nb = json!({"cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(dir.path().join("m.ipynb"), nb.to_string()).unwrap();
    let app = mcp_app(dir.path());
    let sid = mcp_connect(&app, "agent").await;
    let order = |r: String| {
        let mut at: Vec<(usize, char)> = ['a', 'b', 'c']
            .iter()
            .map(|c| (r.find(&format!("src_{c}")).unwrap(), *c))
            .collect();
        at.sort();
        at.into_iter().map(|(_, c)| c).collect::<String>()
    };
    let read = |app: axum::Router, sid: String| async move {
        mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({"path": "m.ipynb"})).await)
    };
    let mv = |after: Value| {
        let (app, sid) = (app.clone(), sid.clone());
        async move {
            mcp_tool(
                &app,
                &sid,
                "cell_move",
                json!({"path": "m.ipynb", "id": "b", "after": after}),
            )
            .await
        }
    };
    assert_eq!(order(read(app.clone(), sid.clone()).await), "abc");
    let out = mcp_text(&mv(json!("b")).await);
    assert!(out.contains("already there"), "{out}");
    assert_eq!(
        order(read(app.clone(), sid.clone()).await),
        "abc",
        "after itself: no-op"
    );
    let v = mv(json!("nope")).await;
    assert!(v.to_string().contains("no cell nope"), "{v}");
    assert_eq!(order(read(app.clone(), sid.clone()).await), "abc");
    mcp_text(&mv(json!("c")).await);
    assert_eq!(order(read(app.clone(), sid.clone()).await), "acb", "down");
    mcp_text(&mv(json!("a")).await);
    assert_eq!(order(read(app.clone(), sid.clone()).await), "abc", "up");
    mcp_text(&mv(Value::Null).await);
    assert_eq!(order(read(app.clone(), sid.clone()).await), "bac", "top");

    let (_, _, res) = mcp_call(
        &app,
        Some(&sid),
        json!({"jsonrpc": "2.0", "id": 90, "method": "resources/read",
        "params": {"uri": "notebook://./m.ipynb"}}),
    )
    .await;
    let text = res["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(order(text), "bac", "{res}");
    let (_, _, res) = mcp_call(
        &app,
        Some(&sid),
        json!({"jsonrpc": "2.0", "id": 91, "method": "resources/read",
        "params": {"uri": "notebook://notes.txt"}}),
    )
    .await;
    assert!(res.get("error").is_some(), "{res}");
}

/// Each connection has its own current notebook, and kernel_control
/// answers what it did.
#[tokio::test]
async fn mcp_connections_and_kernel_control() {
    let dir = tempfile::tempdir().unwrap();
    let app = mcp_app(dir.path());
    let a = mcp_connect(&app, "a").await;
    let b = mcp_connect(&app, "b").await;
    mcp_text(&mcp_tool(&app, &a, "notebook_open", json!({"path": "a.ipynb"})).await);
    mcp_text(&mcp_tool(&app, &b, "notebook_open", json!({"path": "b.ipynb"})).await);
    mcp_text(&mcp_tool(&app, &a, "cell_add", json!({"source": "from a"})).await);
    let read = mcp_text(&mcp_tool(&app, &a, "notebook_read", json!({})).await);
    assert!(read.contains("from a"), "{read}");
    let read = mcp_text(&mcp_tool(&app, &b, "notebook_read", json!({})).await);
    assert!(!read.contains("from a"), "b's notebook is its own: {read}");

    let out = mcp_text(&mcp_tool(&app, &a, "kernel_control", json!({"action": "interrupt"})).await);
    assert!(out.starts_with("Interrupted"), "{out}");
    let out = mcp_text(&mcp_tool(&app, &a, "kernel_control", json!({"action": "shutdown"})).await);
    assert!(out.starts_with("Shut down"), "{out}");
    // Nothing runs now: interrupt and shutdown don't start a kernel.
    let v = mcp_tool(&app, &a, "kernel_control", json!({"action": "interrupt"})).await;
    assert!(v.to_string().contains("no kernel is running"), "{v}");
    let (_, kernels) = call(&app, "GET", "/api/kernels", None, Some(TOKEN)).await;
    assert_eq!(
        kernels.as_array().unwrap().len(),
        1,
        "only b's kernel: {kernels}"
    );
    let out = mcp_text(&mcp_tool(&app, &a, "kernel_control", json!({"action": "restart"})).await);
    assert!(out.starts_with("Restarted"), "{out}");
}

/// The `@@read <mark>` line of a read.
fn read_mark(out: &str) -> String {
    out.lines()
        .rev()
        .find_map(|l| l.strip_prefix("@@read "))
        .unwrap_or_else(|| panic!("no @@read line: {out}"))
        .to_string()
}

/// `notebook_read(since)` lists what a person changed through the room
/// between two agent reads, and not the agent's own edits (#313).
#[tokio::test]
async fn mcp_read_since_lists_changes_by_others() {
    use ganymede_documents::schema::{self, Roots};
    use ganymede_documents::yrs::sync::{Message as YMsg, SyncMessage};
    use ganymede_documents::yrs::updates::decoder::Decode;
    use ganymede_documents::yrs::updates::encoder::Encode;
    use ganymede_documents::yrs::{Array, Doc, Map, Out, ReadTxn, Transact, Update};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};

    let dir = tempfile::tempdir().unwrap();
    let nb = json!({"cells": [
        {"cell_type": "markdown", "id": "a", "metadata": {}, "source": "A"},
        {"cell_type": "code", "id": "b", "execution_count": null, "metadata": {},
         "outputs": [], "source": "src_b"},
        {"cell_type": "markdown", "id": "d", "metadata": {}, "source": "D"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(dir.path().join("j.ipynb"), nb.to_string()).unwrap();
    let app = mcp_app(dir.path());
    let agent = mcp_connect(&app, "agent").await;
    let read = |sid: String, args: Value| {
        let app = app.clone();
        async move { mcp_text(&mcp_tool(&app, &sid, "notebook_read", args).await) }
    };
    let out = read(agent.clone(), json!({"path": "j.ipynb"})).await;
    let mark = read_mark(&out);
    // Nothing changed yet.
    let out = read(agent.clone(), json!({"path": "j.ipynb", "since": mark})).await;
    assert!(out.starts_with("No changes by others"), "{out}");
    assert_eq!(read_mark(&out), mark, "the mark keeps its name");

    // The agent's own edits: a note added, and cell a edited.
    mcp_text(
        &mcp_tool(
            &app,
            &agent,
            "cell_add",
            json!({"path": "j.ipynb", "source": "my note", "after": "a"}),
        )
        .await,
    );
    mcp_text(
        &mcp_tool(
            &app,
            &agent,
            "cell_edit",
            json!({"path": "j.ipynb", "id": "a", "source": "A2"}),
        )
        .await,
    );

    // A person, through the room: edits b, adds p at the end, deletes d.
    let (_, session) = call(
        &app,
        "PUT",
        "/api/collaboration/session/j.ipynb",
        Some(json!({"format": "json", "type": "notebook"})),
        Some(TOKEN),
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    let mut req = format!(
        "ws://{addr}/api/collaboration/room/json:notebook:{}?sessionId={}",
        session["fileId"].as_str().unwrap(),
        session["sessionId"].as_str().unwrap()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let doc = Doc::new();
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::SyncStep1(doc.transact().state_vector()))
            .encode_v1()
            .into(),
    ))
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while Roots::get(&doc).cells.len(&doc.transact()) < 4 {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("room synced")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = msg else { continue };
        if let Ok(YMsg::Sync(SyncMessage::SyncStep2(u) | SyncMessage::Update(u))) =
            YMsg::decode_v1(&b)
        {
            doc.transact_mut()
                .apply_update(Update::decode_v1(&u).unwrap())
                .unwrap();
        }
    }
    let before = doc.transact().state_vector();
    {
        let roots = Roots::get(&doc);
        let mut txn = doc.transact_mut();
        let b = schema::find_cell(&txn, &roots, "b").unwrap();
        let Some(Out::YText(src)) = b.get(&txn, "source") else {
            panic!("source is text")
        };
        schema::set_text(&mut txn, &src, "src_b2");
        let d = schema::cell_index(&txn, &roots, "d").unwrap();
        roots.cells.remove(&mut txn, d);
        let end = roots.cells.len(&txn);
        schema::insert_cell(
            &mut txn,
            &roots.cells,
            end,
            json!({"cell_type": "markdown", "id": "p", "metadata": {}, "source": "P"}),
        );
    }
    let update = doc.transact().encode_state_as_update_v1(&before);
    ws.send(TMsg::Binary(
        YMsg::Sync(SyncMessage::Update(update)).encode_v1().into(),
    ))
    .await
    .unwrap();

    // Wait, from another connection, for the room to have the edit.
    let watcher = mcp_connect(&app, "watcher").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !read(watcher.clone(), json!({"path": "j.ipynb"}))
        .await
        .contains("src_b2")
    {
        assert!(std::time::Instant::now() < deadline, "edit never arrived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A hook's connection asks with the agent's mark.
    let hook = mcp_connect(&app, "hook").await;
    let out = read(hook, json!({"path": "j.ipynb", "since": mark})).await;
    assert!(
        out.starts_with(
            "Changed by others since that read: 1 added, 1 edited, 0 moved, 1 deleted."
        ),
        "{out}"
    );
    assert!(
        out.contains(r#""change":["edited"]"#) && out.contains("src_b2"),
        "{out}"
    );
    assert!(
        out.contains(r#""change":["added"]"#) && out.contains("\nP\n"),
        "{out}"
    );
    assert!(out.contains(r#"@@deleted {"id":"d"}"#), "{out}");
    assert!(
        !out.contains("my note") && !out.contains("A2"),
        "own edits: {out}"
    );
    // That read moved the mark: the agent sees nothing new.
    let out = read(agent.clone(), json!({"path": "j.ipynb", "since": mark})).await;
    assert!(out.starts_with("No changes by others"), "{out}");

    // A hook adds a cell for the agent, passing its mark: not reported.
    let hook = mcp_connect(&app, "hook").await;
    mcp_text(
        &mcp_tool(
            &app,
            &hook,
            "cell_add",
            json!({"path": "j.ipynb", "source": "recorded step", "mode": "recorded", "mark": mark}),
        )
        .await,
    );
    let out = read(agent.clone(), json!({"path": "j.ipynb", "since": mark})).await;
    assert!(out.starts_with("No changes by others"), "{out}");

    // An unknown mark gives the whole notebook and a mark to use next.
    let out = read(agent.clone(), json!({"path": "j.ipynb", "since": "nope"})).await;
    assert!(out.starts_with("Read mark nope is unknown"), "{out}");
    assert!(out.contains("my note") && out.contains("src_b2"), "{out}");
    assert_eq!(read_mark(&out), mark);
    // A mark for another notebook (as after a rename) is as good as none.
    mcp_text(&mcp_tool(&app, &agent, "notebook_open", json!({"path": "k.ipynb"})).await);
    let v = mcp_tool(
        &app,
        &agent,
        "notebook_read",
        json!({"path": "k.ipynb", "since": mark}),
    )
    .await;
    let out = mcp_text(&v);
    assert!(out.starts_with("Read mark"), "{out}");
    assert_ne!(read_mark(&out), mark, "k.ipynb has its own mark");
}

/// A mark read from a new session (as after `ganymede mcp` reconnects)
/// becomes that session's: its edits move it. A failed edit moves
/// nothing, so someone else's change to that cell is still reported
/// (#313).
#[tokio::test]
async fn mcp_read_mark_follows_a_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let nb = json!({"cells": [
        {"cell_type": "markdown", "id": "a", "metadata": {}, "source": "A"},
        {"cell_type": "markdown", "id": "b", "metadata": {}, "source": "B"}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(dir.path().join("n.ipynb"), nb.to_string()).unwrap();
    let app = mcp_app(dir.path());
    let first = mcp_connect(&app, "agent").await;
    let out = mcp_text(&mcp_tool(&app, &first, "notebook_read", json!({"path": "n.ipynb"})).await);
    let mark = read_mark(&out);

    // A new session reads with the mark and no path, then edits.
    let agent = mcp_connect(&app, "agent").await;
    let since = |sid: String| {
        let (app, mark) = (app.clone(), mark.clone());
        async move { mcp_text(&mcp_tool(&app, &sid, "notebook_read", json!({"since": mark})).await) }
    };
    let out = since(agent.clone()).await;
    assert!(out.starts_with("No changes by others"), "{out}");
    mcp_text(
        &mcp_tool(
            &app,
            &agent,
            "cell_add",
            json!({"path": "n.ipynb", "source": "mine"}),
        )
        .await,
    );
    let out = since(agent.clone()).await;
    assert!(out.starts_with("No changes by others"), "{out}");

    // A person deletes b; the agent's edit of b fails and hides nothing.
    let person = mcp_connect(&app, "person").await;
    mcp_text(
        &mcp_tool(
            &app,
            &person,
            "cell_delete",
            json!({"path": "n.ipynb", "id": "b"}),
        )
        .await,
    );
    let v = mcp_tool(
        &app,
        &agent,
        "cell_edit",
        json!({"path": "n.ipynb", "id": "b", "source": "B2"}),
    )
    .await;
    assert!(v.to_string().contains("no cell b"), "{v}");
    let out = since(agent).await;
    assert!(out.contains(r#"@@deleted {"id":"b"}"#), "{out}");
}
