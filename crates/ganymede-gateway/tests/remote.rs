//! A gateway to a real Ganymede server whose kernels are in-process fakes:
//! kernelspecs, start, execute, interrupt, restart and shutdown all go
//! over the remote server's REST API and websocket.

use async_trait::async_trait;
use ganymede_engine::Engine;
use ganymede_gateway::Gateway;
use ganymede_kernel_host::protocol::content::ReplyStatus;
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    in_process, in_process_pair, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner, TransportFactory,
};
use ganymede_server::config::Config;
use ganymede_server::serve::{run_with, Startup};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// The remote server's kernels.
struct Fakes;

impl KernelspecSource for Fakes {
    fn list(&self) -> ganymede_kernel_host::Result<Vec<Kernelspec>> {
        let mut spec: Kernelspec = serde_json::from_value(json!({
            "argv": [],
            "display_name": "Remote fake",
            "language": "fake",
            "interrupt_mode": "message",
        }))?;
        spec.name = "fake".into();
        Ok(vec![spec])
    }
}

#[async_trait]
impl Provisioner for Fakes {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(in_process())
    }
}

#[async_trait]
impl TransportFactory for Fakes {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

/// Starts the remote server; its URL (with the token) and its stopper.
async fn remote(root: &std::path::Path) -> (String, tokio::sync::mpsc::Sender<()>) {
    let cfg = Config {
        port: 0,
        token: Some("secret".into()),
        root_dir: root.into(),
        cookie_secret_file: Some(root.join("cookie")),
        notary_dir: Some(root.join("notary")),
        ..Default::default()
    };
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    let fakes = Arc::new(Fakes);
    tokio::spawn(run_with(
        cfg,
        Startup {
            on_listening: Some(Box::new(move |url: &str| {
                let _ = url_tx.send(url.to_string());
            })),
            stop: Some(stop_rx),
            kernelspecs: Some(fakes.clone()),
            provisioner: Some(fakes.clone()),
            transports: Some(fakes),
            ..Default::default()
        },
    ));
    (url_rx.await.unwrap(), stop_tx)
}

fn base(url: &str) -> String {
    url.split('?').next().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_kernels_on_the_remote_server() {
    let root = tempfile::tempdir().unwrap();
    let (url, stop) = remote(root.path()).await;
    let gateway = Arc::new(Gateway::new(&base(&url), Some("secret".into())).unwrap());
    assert!(gateway.list().unwrap().is_empty(), "nothing before refresh");
    gateway.refresh().await.unwrap();
    let specs = gateway.list().unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "fake");
    assert_eq!(specs[0].display_name, "Remote fake");

    let engine = Engine::builder()
        .kernelspecs(gateway.clone())
        .provisioner(gateway.clone())
        .transports(gateway.clone())
        .build();
    let kernel = Arc::new(engine.start("fake").await.unwrap());
    assert_eq!(gateway.remote_kernels().await.unwrap().len(), 1);
    assert_eq!(kernel.execute("1").await.unwrap().status, ReplyStatus::Ok);

    // An interrupt goes through the remote server's REST API.
    let k = kernel.clone();
    let running = tokio::spawn(async move { k.execute("sleep 30000").await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    kernel.interrupt().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("interrupted")
        .unwrap()
        .unwrap();

    // A restart replaces the remote kernel.
    kernel.restart().await.unwrap();
    assert_eq!(kernel.execute("2").await.unwrap().status, ReplyStatus::Ok);
    assert_eq!(gateway.remote_kernels().await.unwrap().len(), 1);

    kernel.shutdown().await.unwrap();
    assert_eq!(gateway.remote_kernels().await.unwrap().len(), 0);
    stop.send(()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_is_reported() {
    let root = tempfile::tempdir().unwrap();
    let (url, stop) = remote(root.path()).await;
    let gateway = Gateway::new(&base(&url), Some("wrong".into())).unwrap();
    let e = gateway.refresh().await.unwrap_err().to_string();
    assert!(e.contains("403") || e.contains("401"), "{e}");
    stop.send(()).await.unwrap();
}
