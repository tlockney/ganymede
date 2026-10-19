//! `gateway_url`: the server's kernels run on another Jupyter server (here
//! a second Ganymede server with in-process kernels).
#![cfg(feature = "gateway")]

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
use std::time::Duration;
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
async fn call(
    addr: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: token {token}\r\n\
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

/// Starts a server; its `host:port` and stopper.
async fn serve(cfg: Config, app: Option<Arc<App>>) -> (String, tokio::sync::mpsc::Sender<()>) {
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(run_with(
        cfg,
        Startup {
            on_listening: Some(Box::new(move |url: &str| {
                let _ = url_tx.send(url.to_string());
            })),
            stop: Some(stop_rx),
            kernelspecs: app.clone().map(|a| a as _),
            provisioner: app.clone().map(|a| a as _),
            transports: app.map(|a| a as _),
            ..Default::default()
        },
    ));
    let url = url_rx.await.unwrap();
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();
    (addr.to_string(), stop_tx)
}

fn config(root: &std::path::Path, name: &str, token: &str) -> Config {
    Config {
        port: 0,
        token: Some(token.into()),
        root_dir: root.into(),
        cookie_secret_file: Some(root.join(format!("{name}-cookie"))),
        notary_dir: Some(root.join(format!("{name}-notary"))),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn kernels_run_on_the_gateway() {
    let root = tempfile::tempdir().unwrap();
    let (remote, stop_remote) =
        serve(config(root.path(), "remote", "r"), Some(Arc::new(App))).await;
    let mut cfg = config(root.path(), "local", "t");
    cfg.gateway_url = Some(format!("http://{remote}/"));
    cfg.gateway_token = Some("r".into());
    let (local, stop_local) = serve(cfg, None).await;

    // The kernelspecs arrive once the first refresh has run.
    let mut specs = Value::Null;
    for _ in 0..50 {
        specs = call(&local, "t", "GET", "/api/kernelspecs", None).await.1;
        if specs["kernelspecs"]["local"].is_object() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        specs["kernelspecs"]["local"]["spec"]["display_name"], "In-app",
        "{specs}"
    );

    let (code, kernel) = call(
        &local,
        "t",
        "POST",
        "/api/kernels",
        Some(json!({"name": "local"})),
    )
    .await;
    assert_eq!(code, 201, "{kernel}");
    let (_, on_remote) = call(&remote, "r", "GET", "/api/kernels", None).await;
    assert_eq!(on_remote.as_array().unwrap().len(), 1, "{on_remote}");

    let path = format!("/api/kernels/{}", kernel["id"].as_str().unwrap());
    let (code, _) = call(&local, "t", "DELETE", &path, None).await;
    assert_eq!(code, 204);
    let (_, on_remote) = call(&remote, "r", "GET", "/api/kernels", None).await;
    assert_eq!(on_remote.as_array().unwrap().len(), 0, "{on_remote}");

    stop_local.send(()).await.unwrap();
    stop_remote.send(()).await.unwrap();
}
