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
    let (code, kernel) = call(addr, "POST", "/api/kernels", Some(json!({"name": "local"}))).await;
    assert_eq!(code, 201, "{kernel}");
    let path = format!("/api/kernels/{}/restart", kernel["id"].as_str().unwrap());
    let (code, _) = call(addr, "POST", &path, None).await;
    assert_eq!(code, 200);

    stop_tx.send(()).await.unwrap();
    server.await.unwrap().unwrap();
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
