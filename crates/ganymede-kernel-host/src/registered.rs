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
use tokio::sync::{mpsc, oneshot, watch};

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
    /// For an app whose start finishes later: a receiver that yields Ok
    /// once the kernel is started, or Err with why it couldn't be. None
    /// (the default) means `start`'s return already said so.
    fn started(
        &self,
        _kernel_id: &str,
    ) -> Option<oneshot::Receiver<std::result::Result<(), String>>> {
        None
    }
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
    /// content. The host restarts it, counted against its own limit, not the
    /// crash limit.
    Lost,
    /// The process that ran it went away (a provider's connection closed,
    /// #983). The kernel dies with reason `lost` and is never restarted.
    Gone,
    /// The host stopped it.
    Stopped,
}

impl Ended {
    /// `"lost"` or `"exited"`, the C ABI's names; also what
    /// [`KernelProcess::exit_reason`] reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Ended::Exited => "exited",
            // Nothing is left to report the loss to, so a gone process
            // reads as a lost kernel to the host and its clients.
            Ended::Lost | Ended::Gone => "lost",
            Ended::Stopped => "stopped",
        }
    }
}

/// Where a kernelspec came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The embedding process, through the C ABI (#266).
    App,
    /// A process connected over the kernel provider socket (#983).
    Provider,
}

/// The kernels other than the app's: the gateway's, or the host's own.
type Fallback = (
    Arc<dyn KernelspecSource>,
    Arc<dyn Provisioner>,
    Arc<dyn TransportFactory>,
);

/// Kernelspecs the app and its kernel providers registered, and the
/// kernels started from them. An app's are listed after the fallback's,
/// and in place of a namesake there, so the fallback's first stays the
/// default kernel; a provider's go after everything else and never in
/// place of one of the fallback's.
#[derive(Default)]
pub struct RegisteredKernels {
    state: Arc<Mutex<State>>,
    fallback: OnceLock<Fallback>,
}

#[derive(Default)]
struct State {
    specs: Vec<(Kernelspec, Arc<dyn AppKernel>, Origin)>,
    /// By kernel id. A restart replaces the entry under the same id.
    running: HashMap<String, Arc<Running>>,
    /// Launched and not yet connected, by the address of the launch's
    /// process handle, which is how `connect` tells its own launches.
    unconnected: HashMap<usize, Arc<Running>>,
    /// The kernelspec name each kernel a provider started came from, and
    /// that provider, by kernel id (#983): one of those is launched
    /// through its provider again, or not at all, never through the
    /// fallback. Kept when the host stops the kernel, as a restart stops
    /// it before launching it again, and dropped when `end_all_from`
    /// forgets the provider.
    provider_runs: HashMap<String, (String, Arc<dyn AppKernel>)>,
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

    /// The kernels to list before the registered ones (so the default
    /// kernel stays theirs) and to run every other kernelspec. Set once, by the server at startup; without it
    /// only registered kernels exist.
    pub fn set_fallback(
        &self,
        specs: Arc<dyn KernelspecSource>,
        provisioner: Arc<dyn Provisioner>,
        transports: Arc<dyn TransportFactory>,
    ) {
        let _ = self.fallback.set((specs, provisioner, transports));
    }

    /// Lists `spec` as a kernelspec whose kernels `app` runs. A spec from
    /// the app stands in for a fallback's namesake, as the app is the
    /// server's own process; a provider's fails when the fallback lists
    /// the name, since a name that exists elsewhere is not the provider's
    /// to take. Fails too when a spec of that name is registered, whatever
    /// its origin, and then never calls `app`, which is made only once the
    /// name is known to be free.
    pub fn register(
        &self,
        mut spec: Kernelspec,
        origin: Origin,
        app: impl FnOnce() -> Arc<dyn AppKernel>,
    ) -> Result<()> {
        // As kernelspecs on disk are named, so routes and clients match.
        spec.name = spec.name.to_lowercase();
        if !crate::kernelspec::is_valid_kernel_name(&spec.name) {
            return Err(Error::Registration(format!(
                "{:?} is not a kernelspec name: use letters, digits, '.', '_' and '-'",
                spec.name
            )));
        }
        // No process to signal: interrupts go over the control channel.
        spec.interrupt_mode = InterruptMode::Message;
        if origin == Origin::Provider && self.fallback_lists(&spec.name)? {
            return Err(Error::Registration(format!(
                "a kernelspec named {} is installed already, so a provider cannot register it",
                spec.name
            )));
        }
        let mut state = self.state.lock().unwrap();
        if state
            .specs
            .iter()
            .any(|(s, _, _)| s.name.eq_ignore_ascii_case(&spec.name))
        {
            return Err(Error::Registration(format!(
                "a kernelspec named {} is already registered",
                spec.name
            )));
        }
        state.specs.push((spec, app(), origin));
        Ok(())
    }

    /// Forgets the kernelspec `name`, whichever origin registered it.
    /// Running kernels are left alone: a later launch of the name goes to
    /// the fallback, or fails without one. True when one was removed.
    pub fn unregister(&self, name: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        let before = state.specs.len();
        state
            .specs
            .retain(|(s, _, _)| !s.name.eq_ignore_ascii_case(name));
        state.specs.len() != before
    }

    /// Ends every running kernel whose app is `app`, and forgets the specs
    /// that app registered. A provider's connection closing calls it
    /// (#983): there is no one left to run what it started. Returns how
    /// many kernels it ended.
    pub fn end_all_from(&self, app: &Arc<dyn AppKernel>, why: Ended) -> usize {
        let mut state = self.state.lock().unwrap();
        let ids: Vec<String> = state
            .running
            .iter()
            .filter(|(_, r)| Arc::ptr_eq(&r.app, app))
            .map(|(id, _)| id.clone())
            .collect();
        let mut ended = 0;
        for id in ids {
            if let Some(r) = state.running.remove(&id) {
                r.end(why);
                ended += 1;
            }
        }
        state.unconnected.retain(|_, r| !Arc::ptr_eq(&r.app, app));
        // With nothing left to run them again, where they were started
        // from goes too, the host's stopped ones included.
        state.provider_runs.retain(|_, (_, a)| !Arc::ptr_eq(a, app));
        state.specs.retain(|(_, a, _)| !Arc::ptr_eq(a, app));
        ended
    }

    /// Forgets a launch whose start failed or timed out: no kernel to
    /// find, and no pending connection.
    /// Only this launch's run: a later launch may already hold the id. Its
    /// provider run, if any, stays, so a failed restart is still never
    /// relaunched through the fallback.
    fn abandon(&self, kernel_id: &str, process: &Arc<Process>) {
        let mut state = self.state.lock().unwrap();
        if state
            .running
            .get(kernel_id)
            .is_some_and(|r| Arc::ptr_eq(r, &process.run))
        {
            state.running.remove(kernel_id);
        }
        state
            .unconnected
            .remove(&(Arc::as_ptr(process) as *const () as usize));
    }

    /// The kernelspec name kernel `kernel_id` was started from, and the
    /// provider that started it, when a provider did.
    fn provider_run(&self, kernel_id: Option<&str>) -> Option<(String, Arc<dyn AppKernel>)> {
        let id = kernel_id?;
        let state = self.state.lock().unwrap();
        state.provider_runs.get(id).cloned()
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

    fn registered(&self, name: &str) -> Option<(Kernelspec, Arc<dyn AppKernel>, Origin)> {
        self.state
            .lock()
            .unwrap()
            .specs
            .iter()
            .find(|(s, _, _)| s.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Whether the fallback lists a kernelspec `name`, as a provider's
    /// registration and launch both ask.
    fn fallback_lists(&self, name: &str) -> Result<bool> {
        match self.fallback.get() {
            Some((rest, _, _)) => Ok(rest
                .list()?
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(name))),
            None => Ok(false),
        }
    }
}

impl KernelspecSource for RegisteredKernels {
    fn list(&self) -> Result<Vec<Kernelspec>> {
        let mine: Vec<(Kernelspec, Origin)> = self
            .state
            .lock()
            .unwrap()
            .specs
            .iter()
            .map(|(s, _, o)| (s.clone(), *o))
            .collect();
        let rest = match self.fallback.get() {
            Some((rest, _, _)) => rest.list()?,
            None => Vec::new(),
        };
        let listed_by_rest =
            |s: &Kernelspec| rest.iter().any(|r| r.name.eq_ignore_ascii_case(&s.name));
        // An app's spec stands in for the fallback's namesake, in its
        // place, so the fallback's first stays the default kernel. A
        // provider's never does: one the fallback names by now is left
        // out, and the rest go after everything else.
        let mut specs: Vec<Kernelspec> = rest
            .iter()
            .map(|other| {
                mine.iter()
                    .find(|(s, o)| *o == Origin::App && s.name.eq_ignore_ascii_case(&other.name))
                    .map_or_else(|| other.clone(), |(s, _)| s.clone())
            })
            .collect();
        for origin in [Origin::App, Origin::Provider] {
            specs.extend(
                mine.iter()
                    .filter(|(s, o)| *o == origin && !listed_by_rest(s))
                    .map(|(s, _)| s.clone()),
            );
        }
        Ok(specs)
    }
}

#[async_trait]
impl Provisioner for RegisteredKernels {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel> {
        let mine = match self.provider_run(opts.kernel_id.as_deref()) {
            // A kernel a provider started stays its provider's: launched
            // again under the name it started from, or not at all. Neither
            // the fallback nor another origin's spec of that name would run
            // what the kernel was, and another provider that registered the
            // name since must never get a kernel id from this connection.
            Some((name, started_by)) => match self.registered(&name) {
                Some((spec, app, Origin::Provider)) if Arc::ptr_eq(&app, &started_by) => {
                    Some((spec, app, Origin::Provider))
                }
                _ => return Err(Error::NoSuchKernelspec(name)),
            },
            None => {
                let mut mine = self.registered(&spec.name);
                // A name the fallback lists goes to the fallback, even when a
                // provider registered it: the fallback's is the one installed.
                if let Some((_, _, Origin::Provider)) = mine {
                    if self.fallback_lists(&spec.name)? {
                        mine = None;
                    }
                }
                mine
            }
        };
        let Some((spec, app, origin)) = mine else {
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
            if origin == Origin::Provider {
                state
                    .provider_runs
                    .insert(id.clone(), (spec.name.clone(), app.clone()));
            }
        }
        // From here the launch can be dropped while it waits for the app,
        // as when the client that asked for the kernel disconnects. The
        // guard then stops what nobody would connect.
        let mut guard = AbandonOnDrop::new(self, &id, process.clone());
        if let Err(e) = app.start(&id, &spec, &opts) {
            guard.disarm();
            self.abandon(&id, &process);
            return Err(Error::Registration(format!(
                "the app could not start kernel {id}: {e}"
            )));
        }
        // An app that finishes the start later says how it went. No retry:
        // the app knows its own start failed.
        if let Some(ack) = app.started(&id) {
            let wait = opts.startup_timeout.unwrap_or(START_ACK_WAIT);
            match tokio::time::timeout(wait, ack).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => {
                    guard.disarm();
                    self.abandon(&id, &process);
                    return Err(Error::Registration(format!(
                        "the app could not start kernel {id}: {e}"
                    )));
                }
                Ok(Err(_)) => {
                    // The sender went away, so no answer is coming.
                    guard.disarm();
                    self.abandon(&id, &process);
                    return Err(Error::Registration(format!(
                        "the app did not say whether kernel {id} started"
                    )));
                }
                // No answer: the guard forgets the launch and stops the
                // kernel, once.
                Err(_) => {
                    return Err(Error::Registration(format!(
                        "the app did not start kernel {id} within {}s",
                        wait.as_secs()
                    )));
                }
            }
        }
        guard.disarm();
        Ok(LaunchedKernel {
            connection: None,
            connection_file: None,
            process,
            interrupt_mode: InterruptMode::Message,
        })
    }
}

/// Abandons a launch dropped before it finished: cancelled while it waited
/// for the app's acknowledgement, as when the client that asked for the
/// kernel goes away. Left alone, the run would stay listed and
/// unconnected, and the app's kernel would run with nobody to stop it.
struct AbandonOnDrop<'a> {
    host: &'a RegisteredKernels,
    id: &'a str,
    process: Arc<Process>,
    armed: bool,
}

impl<'a> AbandonOnDrop<'a> {
    fn new(host: &'a RegisteredKernels, id: &'a str, process: Arc<Process>) -> Self {
        AbandonOnDrop {
            host,
            id,
            process,
            armed: true,
        }
    }

    /// The launch finished, so its failure is already handled.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AbandonOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.host.abandon(self.id, &self.process);
        self.process.run.app.stop(self.id);
    }
}

/// How long a start waits for an app that finishes it later to say how it
/// went: a provider answers `started` after the fact (#983), and one that
/// never does is no use to anyone. [`LaunchOptions::startup_timeout`]
/// overrides it.
const START_ACK_WAIT: Duration = Duration::from_secs(60);

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
            // Its provider run stays: a restart kills the old run before
            // it launches again under the same id (#984).
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
    /// Not once the process that ran it is gone: nothing is left to run
    /// the kernel again (#983).
    fn restartable(&self) -> bool {
        !matches!(*self.run.ended.borrow(), Some(Ended::Gone))
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
        host.register(spec("app"), Origin::App, || app).unwrap();
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
        let again = host.register(spec("APP"), Origin::App, || {
            called = true;
            unreachable!()
        });
        assert!(matches!(again, Err(Error::Registration(_))));
        assert!(!called);
        assert!(matches!(
            host.register(spec(""), Origin::App, || unreachable!()),
            Err(Error::Registration(_))
        ));
        let listed = host.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].interrupt_mode, InterruptMode::Message);
    }

    #[test]
    fn listed_after_the_fallback_and_in_place_of_its_namesake() {
        let (host, fake) = host();
        let app: Arc<dyn AppKernel> = fake;
        host.register(spec("zed"), Origin::App, || app).unwrap();
        let rest = Arc::new(StaticSpecs(vec![spec("other"), spec("app")]));
        host.set_fallback(rest, Arc::new(Never), Arc::new(Never));
        let listed = host.list().unwrap();
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        // The fallback's first stays first, so it stays the default.
        assert_eq!(names, ["other", "app", "zed"]);
        assert_eq!(listed[1].interrupt_mode, InterruptMode::Message);
    }

    #[test]
    fn names_follow_the_kernelspec_rules() {
        let (host, fake) = host();
        let app: Arc<dyn AppKernel> = fake;
        host.register(spec("App-TS"), Origin::App, || app).unwrap();
        assert!(host.list().unwrap().iter().any(|s| s.name == "app-ts"));
        for bad in ["a/b", "App TS"] {
            assert!(matches!(
                host.register(spec(bad), Origin::App, || unreachable!()),
                Err(Error::Registration(_))
            ));
        }
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

    /// A fake app for `host`, which its kernels answer through.
    fn fake(host: &Arc<RegisteredKernels>) -> Arc<Fake> {
        let fake = Arc::new(Fake {
            host: OnceLock::new(),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let _ = fake.host.set(host.clone());
        fake
    }

    /// Registers `name` as a provider's kernelspec, run by a fake of its own.
    fn provider(host: &Arc<RegisteredKernels>, name: &str) -> Arc<Fake> {
        let fake = fake(host);
        let app: Arc<dyn AppKernel> = fake.clone();
        host.register(spec(name), Origin::Provider, || app).unwrap();
        fake
    }

    #[test]
    fn a_provider_cannot_take_a_name_the_fallback_lists() {
        let host = Arc::new(RegisteredKernels::new());
        let rest = Arc::new(StaticSpecs(vec![spec("other")]));
        host.set_fallback(rest, Arc::new(Never), Arc::new(Never));
        let mut called = false;
        let e = host.register(spec("OTHER"), Origin::Provider, || {
            called = true;
            unreachable!()
        });
        assert!(matches!(e, Err(Error::Registration(_))));
        assert!(!called, "no app is made for a name it cannot have");
        // The app itself may take the name, as it is the server's own
        // process.
        let app: Arc<dyn AppKernel> = fake(&host);
        host.register(spec("other"), Origin::App, || app).unwrap();
    }

    #[test]
    fn provider_specs_list_after_the_fallback_and_never_replace_one() {
        let host = Arc::new(RegisteredKernels::new());
        // A name the fallback starts listing after the provider registered
        // it: the fallback's stands, and the provider's is left out.
        provider(&host, "app");
        provider(&host, "zed");
        let app: Arc<dyn AppKernel> = fake(&host);
        host.register(spec("local"), Origin::App, || app).unwrap();
        let mut installed = spec("app");
        installed.display_name = "Installed".into();
        host.set_fallback(
            Arc::new(StaticSpecs(vec![installed, spec("other")])),
            Arc::new(Never),
            Arc::new(Never),
        );
        let listed = host.list().unwrap();
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["app", "other", "local", "zed"]);
        assert_eq!(
            listed[0].display_name, "Installed",
            "the fallback's spec, not the provider's"
        );
    }

    #[tokio::test]
    async fn a_provider_name_the_fallback_lists_launches_there() {
        let host = Arc::new(RegisteredKernels::new());
        let theirs = provider(&host, "app");
        host.set_fallback(
            Arc::new(StaticSpecs(vec![spec("app")])),
            Arc::new(Never),
            Arc::new(Never),
        );
        let e = host.launch(&spec("app"), LaunchOptions::default()).await;
        assert!(matches!(e, Err(Error::Unsupported("never"))));
        assert_eq!(theirs.starts.load(Ordering::SeqCst), 0);
    }

    /// A kernel a provider started is never launched through the fallback,
    /// even once the provider has unregistered the name it started from: a
    /// name a connection registered and then lost is not the fallback's
    /// (docs/kernel-provider.md). A launch of that name with a new id is,
    /// as before.
    #[tokio::test]
    async fn a_provider_kernel_is_not_relaunched_through_the_fallback() {
        let host = Arc::new(RegisteredKernels::new());
        let theirs = provider(&host, "app");
        let first = host
            .launch(
                &spec("app"),
                LaunchOptions {
                    kernel_id: Some("k1".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(host.unregister("app"));
        host.set_fallback(
            Arc::new(StaticSpecs(vec![spec("app")])),
            Arc::new(Never),
            Arc::new(Never),
        );
        let e = host
            .launch(
                &spec("app"),
                LaunchOptions {
                    kernel_id: Some("k1".into()),
                    ..Default::default()
                },
            )
            .await
            .err()
            .unwrap();
        assert!(
            matches!(&e, Error::NoSuchKernelspec(name) if name == "app"),
            "the fallback must not stand in for the provider: {e}"
        );
        assert_eq!(
            theirs.starts.load(Ordering::SeqCst),
            1,
            "the provider is not called again either"
        );
        assert!(first.process.is_alive(), "the failed launch left it alone");
        // No provider started this one, so the fallback takes it.
        let e = host
            .launch(&spec("app"), LaunchOptions::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(e, Error::Unsupported("never")), "{e}");
    }

    /// A kernel one provider started is never relaunched through another
    /// provider that registered the same name after the first let it go.
    #[tokio::test]
    async fn a_provider_kernel_is_not_relaunched_through_another_provider() {
        let host = Arc::new(RegisteredKernels::new());
        let a = provider(&host, "app");
        let k1 = || LaunchOptions {
            kernel_id: Some("k1".into()),
            ..Default::default()
        };
        let first = host.launch(&spec("app"), k1()).await.unwrap();
        first.process.kill().await.unwrap();
        assert!(host.unregister("app"));
        let b = provider(&host, "app");
        let e = host.launch(&spec("app"), k1()).await.err().unwrap();
        assert!(
            matches!(&e, Error::NoSuchKernelspec(name) if name == "app"),
            "another provider must not get this kernel: {e}"
        );
        assert_eq!(b.starts.load(Ordering::SeqCst), 0);
        assert_eq!(a.starts.load(Ordering::SeqCst), 1);
        // A new kernel of that name is the new provider's, as any is.
        host.launch(&spec("app"), LaunchOptions::default())
            .await
            .unwrap();
        assert_eq!(b.starts.load(Ordering::SeqCst), 1);
    }

    /// A restart stops the old run before it launches again under the same
    /// id, and the relaunch still goes to the provider or nowhere.
    #[tokio::test]
    async fn a_restarted_provider_kernel_is_not_relaunched_through_the_fallback() {
        let host = Arc::new(RegisteredKernels::new());
        let theirs = provider(&host, "app");
        let k1 = || LaunchOptions {
            kernel_id: Some("k1".into()),
            ..Default::default()
        };
        let first = host.launch(&spec("app"), k1()).await.unwrap();
        // As a restart's shutdown does, before the relaunch.
        first.process.kill().await.unwrap();
        let again = host.launch(&spec("app"), k1()).await.unwrap();
        assert_eq!(theirs.starts.load(Ordering::SeqCst), 2);
        again.process.kill().await.unwrap();
        assert!(host.unregister("app"));
        host.set_fallback(
            Arc::new(StaticSpecs(vec![spec("app")])),
            Arc::new(Never),
            Arc::new(Never),
        );
        let e = host.launch(&spec("app"), k1()).await.err().unwrap();
        assert!(
            matches!(&e, Error::NoSuchKernelspec(name) if name == "app"),
            "the fallback must not stand in for the provider: {e}"
        );
        // Once the provider's connection closes, its kernels are forgotten,
        // even after it unregistered the name.
        let app: Arc<dyn AppKernel> = theirs.clone();
        host.end_all_from(&app, Ended::Gone);
        assert!(host.state.lock().unwrap().provider_runs.is_empty());
    }

    #[tokio::test]
    async fn an_unregistered_name_goes_to_the_fallback() {
        let (host, fake) = host();
        host.set_fallback(
            Arc::new(StaticSpecs(vec![])),
            Arc::new(Never),
            Arc::new(Never),
        );
        assert!(host.unregister("app"));
        assert!(!host.unregister("app"), "already gone");
        assert!(host.list().unwrap().is_empty());
        let e = host
            .launch(&spec("app"), LaunchOptions::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(e, Error::Unsupported("never")), "{e}");
        assert_eq!(fake.starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_unregistered_name_without_a_fallback_fails() {
        let (host, _) = host();
        assert!(host.unregister("app"));
        let e = host
            .launch(&spec("app"), LaunchOptions::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(e, Error::NoSuchKernelspec(_)), "{e}");
    }

    /// An app that finishes each start later, answering through the sender
    /// the test takes from `started`.
    struct Deferred {
        acks: Mutex<std::collections::VecDeque<oneshot::Sender<std::result::Result<(), String>>>>,
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    impl AppKernel for Deferred {
        fn start(
            &self,
            _: &str,
            _: &Kernelspec,
            _: &LaunchOptions,
        ) -> std::result::Result<(), String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn started(&self, _: &str) -> Option<oneshot::Receiver<std::result::Result<(), String>>> {
            let (tx, rx) = oneshot::channel();
            self.acks.lock().unwrap().push_back(tx);
            Some(rx)
        }
        fn send(&self, _: &str, _: Channel, _: &Message) {}
        fn stop(&self, _: &str) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn deferred_host() -> (Arc<RegisteredKernels>, Arc<Deferred>) {
        let host = Arc::new(RegisteredKernels::new());
        let app = Arc::new(Deferred {
            acks: Mutex::new(std::collections::VecDeque::new()),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
        });
        let kernel: Arc<dyn AppKernel> = app.clone();
        host.register(spec("app"), Origin::App, || kernel).unwrap();
        (host, app)
    }

    /// Waits for the app to be asked to acknowledge a start, and hands the
    /// test the sender to answer with.
    async fn ack(app: &Deferred) -> oneshot::Sender<std::result::Result<(), String>> {
        for _ in 0..1000 {
            if let Some(tx) = app.acks.lock().unwrap().pop_front() {
                return tx;
            }
            tokio::task::yield_now().await;
        }
        panic!("the app was never asked to acknowledge its start");
    }

    /// Launches `app`'s spec under `k1`, for a test to wait on.
    fn launch_k1(host: &Arc<RegisteredKernels>) -> tokio::task::JoinHandle<Result<LaunchedKernel>> {
        let host = host.clone();
        tokio::spawn(async move {
            host.launch(
                &spec("app"),
                LaunchOptions {
                    kernel_id: Some("k1".into()),
                    ..Default::default()
                },
            )
            .await
        })
    }

    #[tokio::test]
    async fn a_deferred_start_waits_for_its_acknowledgement() {
        let (host, app) = deferred_host();
        let launching = launch_k1(&host);
        let ack = ack(&app).await;
        assert!(
            !launching.is_finished(),
            "start returned before the app said"
        );
        ack.send(Ok(())).unwrap();
        let launched = launching.await.unwrap().unwrap();
        assert!(launched.process.is_alive());
        assert!(host.state.lock().unwrap().running.contains_key("k1"));
    }

    #[tokio::test]
    async fn a_deferred_start_that_fails_leaves_nothing_running() {
        let (host, app) = deferred_host();
        let launching = launch_k1(&host);
        let ack = ack(&app).await;
        ack.send(Err("the page never loaded".into())).unwrap();
        let e = launching.await.unwrap().err().unwrap();
        assert!(e.to_string().contains("the page never loaded"), "{e}");
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert_eq!(app.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            app.stops.load(Ordering::SeqCst),
            0,
            "a start that never happened is not stopped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_deferred_start_that_never_answers_times_out_and_stops() {
        let (host, app) = deferred_host();
        let e = host
            .launch(
                &spec("app"),
                LaunchOptions {
                    kernel_id: Some("k1".into()),
                    ..Default::default()
                },
            )
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("did not start kernel k1"), "{e}");
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert_eq!(
            app.stops.load(Ordering::SeqCst),
            1,
            "a start that timed out is stopped"
        );
    }

    /// The launch waits for the app's acknowledgement for as long as its
    /// `startup_timeout` says, not the host's fallback.
    #[tokio::test(start_paused = true)]
    async fn a_deferred_start_times_out_at_its_startup_timeout() {
        let (host, app) = deferred_host();
        let start = tokio::time::Instant::now();
        let e = tokio::time::timeout(
            Duration::from_secs(7),
            host.launch(
                &spec("app"),
                LaunchOptions {
                    kernel_id: Some("k1".into()),
                    startup_timeout: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("the launch gives up at 5s, not at 60s")
        .err()
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert!(e.to_string().contains("within 5s"), "{e}");
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert_eq!(
            app.stops.load(Ordering::SeqCst),
            1,
            "a start that timed out is stopped"
        );
    }

    /// A launch dropped while it waits for the app to answer, as a client
    /// disconnecting drops it, leaves no run behind and stops the kernel
    /// the app started.
    #[tokio::test]
    async fn a_launch_dropped_while_waiting_for_its_ack_leaves_nothing_and_stops() {
        let (host, app) = deferred_host();
        let launching = launch_k1(&host);
        // Kept, so the launch is waiting for an answer and not for a
        // sender: dropping it would take the other path.
        let _held = ack(&app).await;
        launching.abort();
        let joined = launching.await;
        assert!(
            matches!(joined, Err(e) if e.is_cancelled()),
            "the launch ran on"
        );
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert_eq!(
            app.stops.load(Ordering::SeqCst),
            1,
            "a kernel nobody will connect is stopped"
        );
    }

    /// The app dropped the sender it would have answered with, so no
    /// answer is coming: the start fails and leaves nothing behind.
    #[tokio::test]
    async fn a_deferred_start_whose_sender_is_dropped_fails_and_leaves_nothing() {
        let (host, app) = deferred_host();
        let launching = launch_k1(&host);
        drop(ack(&app).await);
        let e = launching.await.unwrap().err().unwrap();
        assert!(
            e.to_string()
                .contains("did not say whether kernel k1 started"),
            "{e}"
        );
        assert!(host.state.lock().unwrap().running.is_empty());
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert_eq!(
            app.stops.load(Ordering::SeqCst),
            0,
            "a start that never happened is not stopped"
        );
    }

    #[tokio::test]
    async fn end_all_from_ends_only_that_apps_kernels_and_unregisters_its_specs() {
        let (host, fake) = host();
        provider(&host, "b");
        let (_session, ours) = start(&host, "k1").await;
        let theirs = host
            .launch(
                &spec("b"),
                LaunchOptions {
                    kernel_id: Some("k2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let app: Arc<dyn AppKernel> = fake.clone();
        assert_eq!(host.end_all_from(&app, Ended::Gone), 1);
        assert_eq!(ours.process.exit_reason(), Some("lost"));
        assert!(!ours.process.restartable());
        assert!(theirs.process.is_alive(), "the other app's kernel runs on");
        assert!(theirs.process.restartable());
        let listed = host.list().unwrap();
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b"]);
        assert_eq!(
            host.end_all_from(&app, Ended::Gone),
            0,
            "nothing left to end"
        );
    }

    /// `end_all_from` clears a kernel whose launch never connected too, so
    /// no later `connect` finds it, and the app's kernel is over.
    #[tokio::test]
    async fn end_all_from_clears_a_launch_that_never_connected() {
        let host = Arc::new(RegisteredKernels::new());
        let app: Arc<dyn AppKernel> = provider(&host, "b");
        let launched = host
            .launch(
                &spec("b"),
                LaunchOptions {
                    kernel_id: Some("k2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(host.state.lock().unwrap().unconnected.len(), 1);
        assert_eq!(host.end_all_from(&app, Ended::Gone), 1);
        assert!(host.state.lock().unwrap().unconnected.is_empty());
        assert!(!launched.process.is_alive());
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
