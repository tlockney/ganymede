//! Kernels an app runs itself and registers with the host (#266): a
//! kernelspec source, provisioner and transport factory over the app's
//! own start, send and stop, with no process and no ZeroMQ.
//!
//! The Apple apps run their in-app kernel in a web view. Registered here
//! (through `ganymede-ffi`'s C functions), it is listed beside the host's
//! other kernels and runs through the same sessions, rooms and execute
//! endpoint as they do. The design is `docs/in-app-kernel.md`.

use crate::{
    Error, InterruptMode, KernelProcess, KernelTransport, Kernelspec, KernelspecSource,
    LaunchOptions, LaunchedKernel, Provisioner, Result, TransportFactory, TransportSink,
};
use async_trait::async_trait;
use ganymede_protocol::{Channel, Message};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// What the app does for a kernel it runs. Called on the host's threads,
/// never blocking: hand the work to the kernel and return.
pub trait AppKernel: Send + Sync {
    /// Start kernel `kernel_id` from `spec`. Returns once the start is
    /// under way; the host then waits for its `kernel_info_reply`.
    fn start(
        &self,
        kernel_id: &str,
        spec: &Kernelspec,
        opts: &LaunchOptions,
    ) -> std::result::Result<(), String>;
    /// A message for the kernel on shell, control or stdin. Calls for one
    /// kernel don't overlap, and come in the order the host sends them.
    fn send(&self, kernel_id: &str, channel: Channel, msg: &Message);
    /// End the kernel now. A shutdown has already asked it to exit.
    fn stop(&self, kernel_id: &str);
}

/// Why a registered kernel ended without the host asking it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It ended for its own reasons, such as a crash. Counts toward the
    /// restart limit, as any crash does.
    Exited,
    /// The platform ended it, as iOS does to a suspended app's web
    /// content. Restarted without counting toward the limit.
    Lost,
    /// The host stopped it.
    Stopped,
}

impl Ended {
    /// `"lost"` or `"exited"`, the C ABI's names; also what
    /// [`KernelProcess::exit_reason`] reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Ended::Exited => "exited",
            Ended::Lost => "lost",
            Ended::Stopped => "stopped",
        }
    }
}

/// The kernels other than the app's: the gateway's, or the host's own.
type Fallback = (
    Arc<dyn KernelspecSource>,
    Arc<dyn Provisioner>,
    Arc<dyn TransportFactory>,
);

/// Kernelspecs the app registered, listed before the fallback's and
/// hiding one of the same name, and the kernels started from them.
#[derive(Default)]
pub struct RegisteredKernels {
    state: Arc<Mutex<State>>,
    fallback: OnceLock<Fallback>,
}

#[derive(Default)]
struct State {
    specs: Vec<(Kernelspec, Arc<dyn AppKernel>)>,
    /// By kernel id. A restart replaces the entry under the same id.
    running: HashMap<String, Arc<Running>>,
    /// Launched and not yet connected, by the address of the launch's
    /// process handle, which is how `connect` tells its own launches.
    unconnected: HashMap<usize, Arc<Running>>,
}

/// One registered kernel's run.
struct Running {
    id: String,
    app: Arc<dyn AppKernel>,
    /// Messages from the app, in the order it delivered them. Unbounded,
    /// so delivering never blocks the app's thread.
    from_app: mpsc::UnboundedSender<(Channel, Message)>,
    from_app_rx: Mutex<Option<mpsc::UnboundedReceiver<(Channel, Message)>>>,
    ended: watch::Sender<Option<Ended>>,
    /// Held across `send`, so the app's calls for one kernel don't overlap.
    sending: Mutex<()>,
}

impl Running {
    fn end(&self, why: Ended) -> bool {
        self.ended.send_if_modified(|e| {
            if e.is_some() {
                return false;
            }
            *e = Some(why);
            true
        })
    }
}

impl RegisteredKernels {
    pub fn new() -> Self {
        Self::default()
    }

    /// The kernels to list after the registered ones and to run every
    /// other kernelspec. Set once, by the server at startup; without it
    /// only registered kernels exist.
    pub fn set_fallback(
        &self,
        specs: Arc<dyn KernelspecSource>,
        provisioner: Arc<dyn Provisioner>,
        transports: Arc<dyn TransportFactory>,
    ) {
        let _ = self.fallback.set((specs, provisioner, transports));
    }

    /// Lists `spec` as a kernelspec whose kernels `app` runs. Fails when
    /// a kernelspec of that name is already registered, and then never
    /// calls `app`, which is made only once the name is known to be free.
    pub fn register(
        &self,
        mut spec: Kernelspec,
        app: impl FnOnce() -> Arc<dyn AppKernel>,
    ) -> Result<()> {
        if spec.name.is_empty() {
            return Err(Error::Registration("the kernelspec has no name".into()));
        }
        // No process to signal: interrupts go over the control channel.
        spec.interrupt_mode = InterruptMode::Message;
        let mut state = self.state.lock().unwrap();
        if state
            .specs
            .iter()
            .any(|(s, _)| s.name.eq_ignore_ascii_case(&spec.name))
        {
            return Err(Error::Registration(format!(
                "a kernelspec named {} is already registered",
                spec.name
            )));
        }
        state.specs.push((spec, app()));
        Ok(())
    }

    /// A message from kernel `kernel_id`. Never blocks.
    pub fn deliver(&self, kernel_id: &str, channel: Channel, msg: Message) -> Result<()> {
        let running = self.state.lock().unwrap().running.get(kernel_id).cloned();
        match running {
            Some(r) if r.ended.borrow().is_none() => r
                .from_app
                .send((channel, msg))
                .map_err(|_| Error::TransportClosed),
            _ => Err(Error::KernelDied),
        }
    }

    /// Kernel `kernel_id` ended without the host asking. The host sees it
    /// die, and restarts it as its restart policy says. False when no such
    /// kernel is running.
    pub fn ended(&self, kernel_id: &str, why: Ended) -> bool {
        let running = self.state.lock().unwrap().running.remove(kernel_id);
        running.is_some_and(|r| {
            r.end(why);
            true
        })
    }

    fn registered(&self, name: &str) -> Option<(Kernelspec, Arc<dyn AppKernel>)> {
        self.state
            .lock()
            .unwrap()
            .specs
            .iter()
            .find(|(s, _)| s.name.eq_ignore_ascii_case(name))
            .cloned()
    }
}

impl KernelspecSource for RegisteredKernels {
    fn list(&self) -> Result<Vec<Kernelspec>> {
        let mut specs: Vec<Kernelspec> = self
            .state
            .lock()
            .unwrap()
            .specs
            .iter()
            .map(|(s, _)| s.clone())
            .collect();
        if let Some((rest, _, _)) = self.fallback.get() {
            let others: Vec<Kernelspec> = rest
                .list()?
                .into_iter()
                .filter(|o| !specs.iter().any(|s| s.name.eq_ignore_ascii_case(&o.name)))
                .collect();
            specs.extend(others);
        }
        Ok(specs)
    }
}

#[async_trait]
impl Provisioner for RegisteredKernels {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel> {
        let Some((spec, app)) = self.registered(&spec.name) else {
            return match self.fallback.get() {
                Some((_, p, _)) => p.launch(spec, opts).await,
                None => Err(Error::NoSuchKernelspec(spec.name.clone())),
            };
        };
        let id = opts
            .kernel_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let (from_app, rx) = mpsc::unbounded_channel();
        let running = Arc::new(Running {
            id: id.clone(),
            app: app.clone(),
            from_app,
            from_app_rx: Mutex::new(Some(rx)),
            ended: watch::channel(None).0,
            sending: Mutex::new(()),
        });
        let process = Arc::new(Process {
            run: running.clone(),
            registry: Arc::downgrade(&self.state),
        });
        {
            let mut state = self.state.lock().unwrap();
            // A restart under the same id: the old run is over.
            if let Some(old) = state.running.insert(id.clone(), running.clone()) {
                old.end(Ended::Stopped);
            }
            state
                .unconnected
                .insert(Arc::as_ptr(&process) as *const () as usize, running);
        }
        if let Err(e) = app.start(&id, &spec, &opts) {
            let mut state = self.state.lock().unwrap();
            state.running.remove(&id);
            state
                .unconnected
                .remove(&(Arc::as_ptr(&process) as *const () as usize));
            return Err(Error::Registration(format!(
                "the app could not start kernel {id}: {e}"
            )));
        }
        Ok(LaunchedKernel {
            connection: None,
            connection_file: None,
            process,
            interrupt_mode: InterruptMode::Message,
        })
    }
}

/// How many messages from the app wait for the session at most, past the
/// unbounded queue `deliver` writes to.
const INCOMING: usize = 1024;

#[async_trait]
impl TransportFactory for RegisteredKernels {
    async fn connect(&self, launched: &LaunchedKernel) -> Result<KernelTransport> {
        let key = Arc::as_ptr(&launched.process) as *const () as usize;
        let mine = self.state.lock().unwrap().unconnected.remove(&key);
        let Some(running) = mine else {
            return match self.fallback.get() {
                Some((_, _, t)) => t.connect(launched).await,
                None => Err(Error::Unsupported("a kernel the app did not register")),
            };
        };
        let Some(mut from_app) = running.from_app_rx.lock().unwrap().take() else {
            return Err(Error::TransportClosed);
        };
        // The session reads a bounded queue; this keeps `deliver` from
        // ever waiting on it.
        let (to_session, incoming) = mpsc::channel(INCOMING);
        tokio::spawn(async move {
            while let Some(m) = from_app.recv().await {
                if to_session.send(m).await.is_err() {
                    break;
                }
            }
        });
        Ok(KernelTransport {
            sink: Arc::new(Sink(running)),
            incoming,
        })
    }
}

struct Sink(Arc<Running>);

#[async_trait]
impl TransportSink for Sink {
    async fn send(&self, channel: Channel, msg: Message) -> Result<()> {
        if self.0.ended.borrow().is_some() {
            return Err(Error::TransportClosed);
        }
        let _one_at_a_time = self.0.sending.lock().unwrap();
        self.0.app.send(&self.0.id, channel, &msg);
        Ok(())
    }
    async fn heartbeat(&self, _timeout: Duration) -> Result<bool> {
        Ok(self.0.ended.borrow().is_none())
    }
    async fn close(&self) {}
}

/// A registered kernel's stand-in for a process.
struct Process {
    run: Arc<Running>,
    /// To forget the run once the host stops it.
    registry: std::sync::Weak<Mutex<State>>,
}

#[async_trait]
impl KernelProcess for Process {
    fn pid(&self) -> Option<u32> {
        None
    }
    async fn interrupt(&self) -> Result<()> {
        Err(Error::Unsupported(
            "interrupting a registered kernel by signal",
        ))
    }
    async fn kill(&self) -> Result<()> {
        self.kill_now();
        Ok(())
    }
    async fn wait(&self) -> Option<i32> {
        let mut ended = self.run.ended.subscribe();
        let _ = ended.wait_for(Option::is_some).await;
        None
    }
    fn is_alive(&self) -> bool {
        self.run.ended.borrow().is_none()
    }
    fn kill_now(&self) {
        if let Some(state) = self.registry.upgrade() {
            let mut state = state.lock().unwrap();
            state.unconnected.remove(&(self as *const Self as usize));
            // Only this run's entry: a restart may already hold the id.
            if state
                .running
                .get(&self.run.id)
                .is_some_and(|r| Arc::ptr_eq(r, &self.run))
            {
                state.running.remove(&self.run.id);
            }
        }
        if self.run.end(Ended::Stopped) {
            self.run.app.stop(&self.run.id);
        }
    }
    fn exit_reason(&self) -> Option<&'static str> {
        let ended = *self.run.ended.borrow();
        ended.map(Ended::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KernelSession;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers kernel_info and shutdown requests through `deliver`, as an
    /// app's kernel would.
    struct Fake {
        host: OnceLock<Arc<RegisteredKernels>>,
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    impl AppKernel for Fake {
        fn start(
            &self,
            _: &str,
            _: &Kernelspec,
            opts: &LaunchOptions,
        ) -> std::result::Result<(), String> {
            if opts.env.iter().any(|(k, _)| k == "FAIL") {
                return Err("told to fail".into());
            }
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn send(&self, id: &str, channel: Channel, msg: &Message) {
            let reply = match msg.msg_type() {
                "kernel_info_request" => "kernel_info_reply",
                "shutdown_request" => "shutdown_reply",
                _ => return,
            };
            let host = self.host.get().unwrap();
            let status = |state: &str| {
                Message::new(
                    "status",
                    "k",
                    "k",
                    &serde_json::json!({"execution_state": state}),
                )
                .with_parent(msg.header.clone())
            };
            let content = match reply {
                "kernel_info_reply" => serde_json::to_value(crate::testing::kernel_info()).unwrap(),
                _ => serde_json::json!({"status": "ok", "restart": false}),
            };
            host.deliver(id, Channel::IoPub, status("busy")).unwrap();
            host.deliver(id, channel, Message::reply_to(msg, reply, &content))
                .unwrap();
            host.deliver(id, Channel::IoPub, status("idle")).unwrap();
        }
        fn stop(&self, _: &str) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn spec(name: &str) -> Kernelspec {
        serde_json::from_value::<Kernelspec>(serde_json::json!({"argv": [], "display_name": name}))
            .map(|mut s| {
                s.name = name.into();
                s
            })
            .unwrap()
    }

    fn host() -> (Arc<RegisteredKernels>, Arc<Fake>) {
        let host = Arc::new(RegisteredKernels::new());
        let fake = Arc::new(Fake {
            host: OnceLock::new(),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let _ = fake.host.set(host.clone());
        let app: Arc<dyn AppKernel> = fake.clone();
        host.register(spec("app"), || app).unwrap();
        (host, fake)
    }

    async fn start(host: &RegisteredKernels, id: &str) -> (KernelSession, LaunchedKernel) {
        let opts = LaunchOptions {
            kernel_id: Some(id.into()),
            ..Default::default()
        };
        let launched = host.launch(&spec("app"), opts).await.unwrap();
        let transport = host.connect(&launched).await.unwrap();
        (KernelSession::start(transport, Some(&launched)), launched)
    }

    #[test]
    fn a_name_registers_once() {
        let (host, _) = host();
        let mut called = false;
        let again = host.register(spec("APP"), || {
            called = true;
            unreachable!()
        });
        assert!(matches!(again, Err(Error::Registration(_))));
        assert!(!called);
        assert!(matches!(
            host.register(spec(""), || unreachable!()),
            Err(Error::Registration(_))
        ));
        let listed = host.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].interrupt_mode, InterruptMode::Message);
    }

    #[test]
    fn listed_before_the_fallback_and_hiding_its_namesake() {
        let (host, _) = host();
        let rest = Arc::new(StaticSpecs(vec![spec("app"), spec("other")]));
        host.set_fallback(rest, Arc::new(Never), Arc::new(Never));
        let names: Vec<String> = host.list().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["app", "other"]);
    }

    #[tokio::test]
    async fn runs_through_the_app() {
        let (host, fake) = host();
        let (session, launched) = start(&host, "k1").await;
        session.nudge(Duration::from_secs(5)).await.unwrap();
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
        session
            .shutdown(false, Duration::from_secs(1))
            .await
            .unwrap();
        launched.process.kill().await.unwrap();
        assert_eq!(fake.stops.load(Ordering::SeqCst), 1);
        assert!(!launched.process.is_alive());
        assert_eq!(launched.process.exit_reason(), Some("stopped"));
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(matches!(
            host.deliver("k1", Channel::IoPub, Message::new("status", "", "", &())),
            Err(Error::KernelDied)
        ));
    }

    #[tokio::test]
    async fn stopping_an_old_run_keeps_its_restart() {
        let (host, fake) = host();
        let (_, first) = start(&host, "k1").await;
        let (session, _second) = start(&host, "k1").await;
        assert_eq!(first.process.exit_reason(), Some("stopped"));
        first.process.kill().await.unwrap();
        assert_eq!(fake.stops.load(Ordering::SeqCst), 0, "already over");
        assert!(host.state.lock().unwrap().running.contains_key("k1"));
        session.nudge(Duration::from_secs(5)).await.unwrap();
    }

    #[tokio::test]
    async fn the_app_says_it_ended() {
        let (host, fake) = host();
        let (session, launched) = start(&host, "k1").await;
        session.nudge(Duration::from_secs(5)).await.unwrap();
        assert!(host.ended("k1", Ended::Lost));
        assert!(!host.ended("k1", Ended::Lost), "already ended");
        tokio::time::timeout(Duration::from_secs(5), launched.process.wait())
            .await
            .unwrap();
        assert_eq!(launched.process.exit_reason(), Some("lost"));
        // Ended by the app, so the host doesn't stop it again.
        launched.process.kill().await.unwrap();
        assert_eq!(fake.stops.load(Ordering::SeqCst), 0);
        assert!(matches!(
            host.deliver("k1", Channel::IoPub, Message::new("status", "", "", &())),
            Err(Error::KernelDied)
        ));
    }

    #[tokio::test]
    async fn a_failed_start_leaves_nothing_running() {
        let (host, _) = host();
        let opts = LaunchOptions {
            kernel_id: Some("k1".into()),
            env: vec![("FAIL".into(), "1".into())],
            ..Default::default()
        };
        let e = host.launch(&spec("app"), opts).await.err().unwrap();
        assert!(e.to_string().contains("told to fail"), "{e}");
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
    }

    #[tokio::test]
    async fn other_kernelspecs_go_to_the_fallback() {
        let (host, _) = host();
        let e = host.launch(&spec("other"), LaunchOptions::default()).await;
        assert!(matches!(e, Err(Error::NoSuchKernelspec(_))));
        host.set_fallback(
            Arc::new(StaticSpecs(vec![])),
            Arc::new(Never),
            Arc::new(Never),
        );
        let e = host.launch(&spec("other"), LaunchOptions::default()).await;
        assert!(matches!(e, Err(Error::Unsupported("never"))));
    }

    struct StaticSpecs(Vec<Kernelspec>);

    impl KernelspecSource for StaticSpecs {
        fn list(&self) -> Result<Vec<Kernelspec>> {
            Ok(self.0.clone())
        }
    }

    struct Never;

    #[async_trait]
    impl Provisioner for Never {
        async fn launch(&self, _: &Kernelspec, _: LaunchOptions) -> Result<LaunchedKernel> {
            Err(Error::Unsupported("never"))
        }
    }

    #[async_trait]
    impl TransportFactory for Never {
        async fn connect(&self, _: &LaunchedKernel) -> Result<KernelTransport> {
            Err(Error::Unsupported("never"))
        }
    }
}
