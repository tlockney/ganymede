//! A kernel the app runs itself, with no ZeroMQ and no subprocess: the
//! iPadOS profile. Passes with `--no-default-features`.

use async_trait::async_trait;
use ganymede_engine::Engine;
use ganymede_kernel_host::protocol::content::ReplyStatus;
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    in_process, in_process_pair, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner, TransportFactory,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct OneSpec;
impl KernelspecSource for OneSpec {
    fn list(&self) -> ganymede_kernel_host::Result<Vec<Kernelspec>> {
        let mut spec: Kernelspec = serde_json::from_value(serde_json::json!({
            "argv": [],
            "display_name": "In-process",
            "language": "fake",
        }))?;
        spec.name = "local".into();
        Ok(vec![spec])
    }
}

/// Launches by doing nothing: the factory starts the kernel when asked
/// for its transport, as an app's own kernel thread would.
#[derive(Default)]
struct App {
    launches: AtomicUsize,
}

#[async_trait]
impl Provisioner for App {
    async fn launch(
        &self,
        _spec: &Kernelspec,
        _opts: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        Ok(in_process())
    }
}

#[async_trait]
impl TransportFactory for App {
    async fn connect(
        &self,
        launched: &LaunchedKernel,
    ) -> ganymede_kernel_host::Result<KernelTransport> {
        assert!(launched.connection.is_none());
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

#[tokio::test]
async fn starts_runs_and_restarts_without_zeromq() {
    let app = Arc::new(App::default());
    let engine = Engine::builder()
        .kernelspecs(Arc::new(OneSpec))
        .provisioner(app.clone())
        .transports(app.clone())
        .build();
    let kernel = engine.start("local").await.unwrap();
    assert!(kernel.connection_info().is_none());
    assert_eq!(kernel.execute("1").await.unwrap().status, ReplyStatus::Ok);
    kernel.restart().await.unwrap();
    assert_eq!(app.launches.load(Ordering::SeqCst), 2);
    assert_eq!(kernel.execute("2").await.unwrap().status, ReplyStatus::Ok);
    kernel.shutdown().await.unwrap();
}

#[cfg(not(feature = "zmq"))]
#[tokio::test]
async fn without_zeromq_a_factory_is_required() {
    let engine = Engine::builder()
        .kernelspecs(Arc::new(OneSpec))
        .provisioner(Arc::new(App::default()))
        .build();
    let e = engine.start("local").await.err().unwrap();
    assert!(e.to_string().contains("transport factory"), "{e}");
}
