//! Running kernels by id, and the models for them.

use crate::{
    now_iso, ApiError, KernelModel, KernelspecModel, KernelspecSpec, KernelspecsModel, Result,
};
use ganymede_kernel_host::protocol::{Channel, Header, Message};
use ganymede_kernel_host::{
    Error as HostError, KernelSession, Kernelspec, KernelspecSource, LaunchOptions, LaunchedKernel,
    Provisioner,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// Launches tried before a kernel that never answers counts as failed.
const START_ATTEMPTS: u32 = 3;

/// Unanswered shell requests remembered per kernel.
const UNANSWERED_LIMIT: usize = 1000;

/// A kernel that runs this long after a restart has started cleanly, and
/// its next crash counts from one again (`KernelRestarter.stable_start_time`).
const STABLE_START: Duration = Duration::from_secs(10);

#[cfg(feature = "zmq")]
pub use ganymede_kernel_host::zmq::ZmqTransportFactory;
/// How a launched kernel gets its transport. ZMQ in the server; an
/// in-process pair in an app or a test.
pub use ganymede_kernel_host::TransportFactory;

/// A fixed list of kernelspecs. The iPad profile's source, and the test
/// double.
pub struct StaticKernelspecs(pub Vec<Kernelspec>);

impl KernelspecSource for StaticKernelspecs {
    fn list(&self) -> ganymede_kernel_host::Result<Vec<Kernelspec>> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone)]
pub struct KernelManagerConfig {
    /// Reported as `default` in `/api/kernelspecs`. When unset, the first
    /// listed kernelspec is reported, because clients require a value.
    pub default_kernel: Option<String>,
    pub startup_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Discard kernel processes' own stdout and stderr.
    pub quiet: bool,
    /// Shut down kernels idle this long with no connected clients.
    pub cull_idle: Option<Duration>,
    /// Restarts in a row after a kernel crashes before it is left dead,
    /// as `KernelRestarter.restart_limit`. A crash more than ten seconds
    /// after the last restart counts from one again. 0 never restarts.
    pub restart_limit: u32,
    /// Working directories for kernels are resolved under this root.
    pub root_dir: PathBuf,
    /// URL prefix for kernelspec resource links, as `jupyter_server`
    /// reports them. Ends with `/`.
    pub base_url: String,
}

impl Default for KernelManagerConfig {
    fn default() -> Self {
        KernelManagerConfig {
            default_kernel: None,
            startup_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(5),
            quiet: false,
            cull_idle: None,
            restart_limit: 5,
            root_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            base_url: "/".into(),
        }
    }
}

/// One kernel the manager owns. `clients` queues every message for each
/// client and outlives restarts, so a client's connection survives the
/// session being replaced.
pub struct RunningKernel {
    pub id: String,
    pub name: String,
    pub spec: Kernelspec,
    pub started: String,
    /// Working directory, kept so a restart runs where the first launch did.
    cwd: Option<PathBuf>,
    session: RwLock<KernelSession>,
    launched: Mutex<Option<LaunchedKernel>>,
    pub(crate) clients: Mutex<crate::bridge::Clients>,
    /// False once the manager has shut the kernel down. Clients hold an
    /// `Arc` to this struct, so their queues never close; they watch this
    /// instead.
    alive: watch::Sender<bool>,
    /// Set while a start or restart replaces the session, so the death of
    /// a session being replaced is not reported as the kernel's.
    booting: AtomicBool,
    /// Held across start, restart, and shutdown, so they do not interleave.
    /// In an `Arc` so the start's background boot can own its guard.
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    last_activity: Mutex<String>,
    execution_state: Mutex<String>,
    /// Shell requests clients have sent that the current session has not
    /// answered, oldest first. Answered for the kernel if it crashes.
    unanswered: Mutex<VecDeque<Header>>,
    /// Restarts after crashes in a row, and when the last one was.
    crashes: Mutex<(u32, Option<tokio::time::Instant>)>,
    /// A client asked the current session's kernel to exit with a
    /// `shutdown_request`, so its exit is not a crash.
    asked_to_exit: AtomicBool,
}

impl std::fmt::Debug for RunningKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningKernel")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

impl RunningKernel {
    pub fn session(&self) -> KernelSession {
        self.session.read().unwrap().clone()
    }

    pub fn model(&self) -> KernelModel {
        KernelModel {
            id: self.id.clone(),
            name: self.name.clone(),
            last_activity: self.last_activity.lock().unwrap().clone(),
            execution_state: self.execution_state.lock().unwrap().clone(),
            connections: self.clients.lock().unwrap().connections(),
        }
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }

    pub fn watch_alive(&self) -> watch::Receiver<bool> {
        self.alive.subscribe()
    }

    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = now_iso();
    }

    fn set_state(&self, state: &str) {
        *self.execution_state.lock().unwrap() = state.to_string();
    }

    /// Pump one session's traffic to the clients, tracking activity, until
    /// the session dies. The task holds neither the kernel nor the session,
    /// so a kernel that is shut down and dropped is freed.
    fn pump(self: &Arc<Self>, session: &KernelSession, core: Weak<Core>) {
        let mut rx = session.subscribe();
        let mut alive = session.watch_alive();
        let session_id = session.session_id().to_string();
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let got = tokio::select! {
                    biased;
                    got = rx.recv() => got,
                    _ = alive.wait_for(|a| !*a) => break,
                };
                let Some(k) = me.upgrade() else { return };
                match got {
                    Ok((channel, msg)) => k.forward(channel, msg),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(kernel = %k.id, "pump lagged by {n} messages; clients missed them")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // What the kernel sent before it died still goes to clients.
            loop {
                match rx.try_recv() {
                    Ok((channel, msg)) => match me.upgrade() {
                        Some(k) => k.forward(channel, msg),
                        None => return,
                    },
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            if let (Some(k), Some(core)) = (me.upgrade(), core.upgrade()) {
                k.session_died(&session_id, &core);
            }
        });
    }

    fn forward(&self, channel: Channel, msg: Message) {
        self.touch();
        if channel == Channel::Shell {
            if let Some(parent) = msg.parent_id() {
                self.unanswered
                    .lock()
                    .unwrap()
                    .retain(|h| h.msg_id != parent);
            }
        }
        if channel == Channel::IoPub && msg.msg_type() == "status" && tracks_state(&msg) {
            if let Some(s) = msg.content.get("execution_state").and_then(|v| v.as_str()) {
                self.set_state(s);
            }
        }
        self.clients
            .lock()
            .unwrap()
            .dispatch(&self.id, channel, &msg);
    }

    /// A client sent a `shutdown_request`: the kernel will exit, and that
    /// is not a crash. With `restart: true` too, since the client that
    /// sent it is the one starting the next kernel, as `ganymede-engine`
    /// does through a gateway.
    pub(crate) fn asked_to_exit(&self) {
        self.asked_to_exit.store(true, Ordering::SeqCst);
    }

    /// A shell request a client sent, to be answered for the kernel if it
    /// crashes first.
    pub(crate) fn sent_on_shell(&self, header: &Header) {
        let mut unanswered = self.unanswered.lock().unwrap();
        // Requests a kernel never answers (an unknown type, which kernels
        // ignore) must not pile up for ever.
        if unanswered.len() == UNANSWERED_LIMIT {
            unanswered.pop_front();
        }
        unanswered.push_back(header.clone());
    }

    /// The session died. Unless the manager shut it down or a start or
    /// restart is replacing it, the kernel crashed.
    fn session_died(self: &Arc<Self>, session_id: &str, core: &Arc<Core>) {
        if self.booting.load(Ordering::SeqCst) || self.session().session_id() != session_id {
            return;
        }
        if self.asked_to_exit.load(Ordering::SeqCst) {
            self.remove_dead(&core.kernels);
            return;
        }
        tokio::spawn(core.clone().crashed(self.clone(), session_id.to_string()));
    }

    /// Answer the shell requests the crashed kernel left unanswered, so
    /// clients stop waiting for them: the one it was running gets an
    /// error (and an `error` output, for a cell), the ones queued behind
    /// it are aborted, as a kernel aborts them after an error. Each is
    /// followed by `status: idle`, which a client also waits for.
    fn answer_for_crashed_kernel(&self, why: &str) {
        let unanswered: Vec<Header> = self.unanswered.lock().unwrap().drain(..).collect();
        let mut clients = self.clients.lock().unwrap();
        for (i, parent) in unanswered.into_iter().enumerate() {
            let reply_type = match parent.msg_type.strip_suffix("_request") {
                Some(base) => format!("{base}_reply"),
                None => continue,
            };
            let msg = |msg_type: &str, content: serde_json::Value| {
                Message::new(msg_type, "server", "server", &content).with_parent(parent.clone())
            };
            let content = if i == 0 {
                let error = serde_json::json!({
                    "ename": "KernelDied",
                    "evalue": why,
                    "traceback": [format!("KernelDied: {why}")],
                });
                if parent.msg_type == "execute_request" {
                    clients.dispatch(&self.id, Channel::IoPub, &msg("error", error.clone()));
                }
                let mut reply = error;
                reply["status"] = "error".into();
                reply
            } else {
                serde_json::json!({"status": "aborted"})
            };
            clients.dispatch(&self.id, Channel::Shell, &msg(&reply_type, content));
            clients.dispatch(
                &self.id,
                Channel::IoPub,
                &msg("status", serde_json::json!({"execution_state": "idle"})),
            );
        }
    }

    /// Drop a dead kernel from the map, tell clients `status: dead`, and
    /// close them, as `jupyter_server` does when a kernel dies and is not
    /// restarted.
    fn remove_dead(self: &Arc<Self>, kernels: &KernelMap) {
        {
            let mut map = kernels.write().unwrap();
            match map.get(&self.id) {
                Some(k) if Arc::ptr_eq(k, self) => map.remove(&self.id),
                // Already removed by `shutdown`, which reports it.
                _ => return,
            };
        }
        tracing::warn!(kernel = %self.id, "kernel died");
        self.set_state("dead");
        self.announce("status", serde_json::json!({"execution_state": "dead"}));
        self.alive.send_replace(false);
        self.clients.lock().unwrap().discard_parked(&self.id);
        self.launched.lock().unwrap().take();
    }

    /// A message the server itself injects, such as `status: restarting`.
    fn announce(&self, msg_type: &str, content: serde_json::Value) {
        let msg = Message::new(msg_type, "server", "server", &content);
        self.clients
            .lock()
            .unwrap()
            .dispatch(&self.id, Channel::IoPub, &msg);
    }
}

type KernelMap = RwLock<HashMap<String, Arc<RunningKernel>>>;

/// What launching a kernel takes, shared with the task that pumps each
/// kernel's messages, so that task can launch it again after a crash.
struct Core {
    provisioner: Arc<dyn Provisioner>,
    transports: Arc<dyn TransportFactory>,
    kernels: KernelMap,
    config: KernelManagerConfig,
}

pub struct KernelManager {
    specs: Arc<dyn KernelspecSource>,
    core: Arc<Core>,
    pub started: String,
}

impl KernelManager {
    pub fn new(
        specs: Arc<dyn KernelspecSource>,
        provisioner: Arc<dyn Provisioner>,
        transports: Arc<dyn TransportFactory>,
        config: KernelManagerConfig,
    ) -> Self {
        KernelManager {
            specs,
            core: Arc::new(Core {
                provisioner,
                transports,
                kernels: RwLock::new(HashMap::new()),
                config,
            }),
            started: now_iso(),
        }
    }

    pub fn config(&self) -> &KernelManagerConfig {
        &self.core.config
    }

    // ----- kernelspecs -------------------------------------------------

    pub fn kernelspecs(&self) -> Result<KernelspecsModel> {
        let specs = self.specs.list()?;
        let default = self
            .core
            .config
            .default_kernel
            .clone()
            .or_else(|| specs.first().map(|s| s.name.clone()))
            .unwrap_or_default();
        Ok(KernelspecsModel {
            default,
            kernelspecs: specs
                .into_iter()
                .map(|s| (s.name.clone(), spec_model(&s, &self.core.config.base_url)))
                .collect(),
        })
    }

    /// The directory a kernelspec's resource files (logos) live in.
    pub fn kernelspec_resource_dir(&self, name: &str) -> Result<PathBuf> {
        Ok(self.specs.get(name)?.resource_dir)
    }

    pub fn kernelspec(&self, name: &str) -> Result<KernelspecModel> {
        Ok(spec_model(
            &self.specs.get(name)?,
            &self.core.config.base_url,
        ))
    }

    // ----- kernels -----------------------------------------------------

    pub fn list(&self) -> Vec<KernelModel> {
        let mut v: Vec<_> = self
            .core
            .kernels
            .read()
            .unwrap()
            .values()
            .map(|k| k.model())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn get(&self, id: &str) -> Result<Arc<RunningKernel>> {
        self.core
            .kernels
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("kernel {id}")))
    }

    pub fn model(&self, id: &str) -> Result<KernelModel> {
        Ok(self.get(id)?.model())
    }

    /// Launch a kernel. `path` is the API path of the directory to run it
    /// in, as `POST /api/kernels` takes it; a session passes its notebook's
    /// directory.
    pub async fn start(&self, name: Option<&str>, path: Option<&str>) -> Result<KernelModel> {
        let name = match name {
            Some(n) => n.to_string(),
            None => self.kernelspecs()?.default,
        };
        if name.is_empty() {
            return Err(ApiError::bad_request(
                "no kernel name given and no kernelspecs installed",
            ));
        }
        let spec = self.specs.get(&name)?;
        let id = uuid::Uuid::new_v4().to_string();
        // The kernel runs in the notebook's directory, or the nearest
        // existing directory above it, or the contents root, as it does
        // under jupyter_server. A path that would leave the root gives
        // the root.
        let cwd = Some(match path {
            Some(p) => ganymede_contents::kernel_cwd(&self.core.config.root_dir, p),
            None => self.core.config.root_dir.clone(),
        });
        let (session, launched) = self.core.launch(&spec, cwd.clone(), &id).await?;
        let kernel = Arc::new(RunningKernel {
            id: id.clone(),
            name: spec.name.clone(),
            spec,
            started: now_iso(),
            cwd,
            session: RwLock::new(session.clone()),
            launched: Mutex::new(Some(launched)),
            clients: Mutex::default(),
            alive: watch::channel(true).0,
            booting: AtomicBool::new(true),
            lifecycle: Arc::default(),
            last_activity: Mutex::new(now_iso()),
            execution_state: Mutex::new("starting".into()),
            unanswered: Mutex::default(),
            crashes: Mutex::default(),
            asked_to_exit: AtomicBool::new(false),
        });
        let guard = kernel.lifecycle.clone().lock_owned().await;
        kernel.pump(&session, Arc::downgrade(&self.core));
        self.core
            .kernels
            .write()
            .unwrap()
            .insert(id.clone(), kernel.clone());
        // Answer once the process is running, as jupyter_server does: its
        // start returns before the kernel's first reply. The handshake runs
        // in the background, holding the lifecycle lock so a shutdown or
        // restart waits for it, and clients can connect meanwhile. A kernel
        // that never answers is reported dead and removed, as one that
        // dies later is.
        let (core, booting) = (self.core.clone(), kernel.clone());
        tokio::spawn(async move {
            let _guard = guard;
            let _ = core.boot(&booting).await;
        });
        Ok(kernel.model())
    }

    /// Wait for a start or restart of `id` in progress, and its model once
    /// the kernel has answered. 404 when the kernel did not come up: it was
    /// reported dead and removed.
    pub async fn wait_started(&self, id: &str) -> Result<KernelModel> {
        let kernel = self.get(id)?;
        let _guard = kernel.lifecycle.lock().await;
        self.core.ensure_listed(&kernel)?;
        Ok(kernel.model())
    }

    pub async fn shutdown(&self, id: &str) -> Result<()> {
        let kernel = self.get(id)?;
        let _guard = kernel.lifecycle.lock().await;
        self.core.ensure_listed(&kernel)?;
        self.core.kernels.write().unwrap().remove(id);
        let result = kernel
            .session()
            .shutdown(false, self.core.config.shutdown_grace)
            .await;
        kernel.set_state("dead");
        kernel.alive.send_replace(false);
        kernel.clients.lock().unwrap().discard_parked(&kernel.id);
        kernel.launched.lock().unwrap().take();
        // The kernel is gone from the map and marked dead whatever the
        // escalation reported, so the delete succeeded, as jupyter_server
        // answers it (#171).
        if let Err(e) = result {
            tracing::warn!(kernel = %id, "kernel removed, but shutdown reported: {e}");
        }
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> Result<()> {
        let kernel = self.get(id)?;
        kernel.session().interrupt().await?;
        kernel.touch();
        Ok(())
    }

    /// Same id, same client queues, new process. Clients see `status: restarting`
    /// as they do from `jupyter_server`.
    /// On failure the kernel is reported dead and removed.
    pub async fn restart(&self, id: &str) -> Result<KernelModel> {
        let kernel = self.get(id)?;
        let _guard = kernel.lifecycle.lock().await;
        self.core.ensure_listed(&kernel)?;
        kernel.booting.store(true, Ordering::SeqCst);
        kernel.announce(
            "status",
            serde_json::json!({"execution_state": "restarting"}),
        );
        kernel.set_state("restarting");
        let old = kernel.session();
        let _ = old.shutdown(true, self.core.config.shutdown_grace).await;
        // The new kernel reuses the id, so its connection file has the same
        // path. Dropping the old launch removes that file; do it before the
        // new kernel can read it, not after.
        kernel.launched.lock().unwrap().take();
        if let Err(e) = self.core.relaunch(&kernel).await {
            tracing::error!(kernel = %id, "kernel failed to restart: {e}");
            kernel.booting.store(false, Ordering::SeqCst);
            kernel.remove_dead(&self.core.kernels);
            return Err(e);
        }
        self.core.boot(&kernel).await?;
        kernel.touch();
        Ok(kernel.model())
    }

    /// Shut down kernels idle longer than the configured limit with no
    /// clients connected. Returns the ids culled.
    pub async fn cull_idle(&self) -> Vec<String> {
        let Some(limit) = self.core.config.cull_idle else {
            return vec![];
        };
        let now = chrono::Utc::now();
        let candidates: Vec<String> = self
            .core
            .kernels
            .read()
            .unwrap()
            .values()
            .filter(|k| {
                k.clients.lock().unwrap().connections() == 0
                    && k.execution_state.lock().unwrap().as_str() == "idle"
            })
            .filter(|k| {
                chrono::DateTime::parse_from_rfc3339(&k.last_activity.lock().unwrap())
                    .map(|t| now.signed_duration_since(t).to_std().unwrap_or_default() > limit)
                    .unwrap_or(false)
            })
            .map(|k| k.id.clone())
            .collect();
        let mut culled = vec![];
        for id in candidates {
            tracing::info!(kernel = %id, "culling idle kernel");
            if self.shutdown(&id).await.is_ok() {
                culled.push(id);
            }
        }
        culled
    }

    /// Shut every kernel down at once, so N hung kernels take one grace
    /// period, not N, against a service manager's stop timeout.
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = self.core.kernels.read().unwrap().keys().cloned().collect();
        futures::future::join_all(ids.iter().map(|id| self.shutdown(id))).await;
    }

    pub fn connections(&self) -> usize {
        self.core
            .kernels
            .read()
            .unwrap()
            .values()
            .map(|k| k.clients.lock().unwrap().connections())
            .sum()
    }

    pub fn last_activity(&self) -> String {
        self.core
            .kernels
            .read()
            .unwrap()
            .values()
            .map(|k| k.last_activity.lock().unwrap().clone())
            .max()
            .unwrap_or_else(|| self.started.clone())
    }
}

impl Core {
    /// `kernel`'s session `session_id` died with nothing asking it to.
    /// Restart the kernel under the same id, as `jupyter_client`'s
    /// `KernelRestarter` does: clients see `status: restarting`, then the
    /// new kernel's own statuses. After `restart_limit` crashes in a row,
    /// each within [`STABLE_START`] of the last restart, it is left dead.
    async fn crashed(self: Arc<Self>, kernel: Arc<RunningKernel>, session_id: String) {
        let _guard = kernel.lifecycle.lock().await;
        // A shutdown or restart that held the lock first has already
        // dealt with this session.
        if self.ensure_listed(&kernel).is_err() || kernel.session().session_id() != session_id {
            return;
        }
        let count = {
            let mut crashes = kernel.crashes.lock().unwrap();
            let now = tokio::time::Instant::now();
            crashes.0 = match crashes.1 {
                Some(last) if now - last < STABLE_START => crashes.0 + 1,
                _ => 1,
            };
            crashes.1 = Some(now);
            crashes.0
        };
        let limit = self.config.restart_limit;
        if count > limit {
            if limit > 0 {
                tracing::error!(kernel = %kernel.id, "kernel crashed {count} times in a row; leaving it dead");
            }
            kernel.answer_for_crashed_kernel("The kernel died.");
            kernel.remove_dead(&self.kernels);
            return;
        }
        tracing::warn!(kernel = %kernel.id, "kernel died; restarting it ({count}/{limit})");
        kernel.booting.store(true, Ordering::SeqCst);
        kernel.answer_for_crashed_kernel("The kernel died, and is being restarted.");
        kernel.announce(
            "status",
            serde_json::json!({"execution_state": "restarting"}),
        );
        kernel.set_state("restarting");
        kernel.launched.lock().unwrap().take();
        if let Err(e) = self.relaunch(&kernel).await {
            tracing::error!(kernel = %kernel.id, "kernel failed to restart: {e}");
            kernel.booting.store(false, Ordering::SeqCst);
            kernel.remove_dead(&self.kernels);
            return;
        }
        // On failure `boot` reports the kernel dead and removes it.
        if self.boot(&kernel).await.is_ok() {
            kernel.touch();
        }
    }

    /// Wait for the kernel's new session to answer, and launch it again on
    /// fresh ports when it dies or never answers: ports are free when
    /// picked but belong to whichever process binds them first, so two
    /// kernels launched at once can be given the same one. The engine's
    /// `Relaunch::start` does the same. Runs with `booting` set, under the
    /// lifecycle lock. On failure the kernel is reported dead and removed.
    async fn boot(self: &Arc<Self>, kernel: &Arc<RunningKernel>) -> Result<()> {
        let mut attempt = 1;
        let result = loop {
            let session = kernel.session();
            let e = match session.nudge(self.config.startup_timeout).await {
                Ok(_) => break Ok(()),
                Err(e) => e,
            };
            let _ = session.shutdown(false, Duration::from_millis(200)).await;
            kernel.launched.lock().unwrap().take();
            let retry = matches!(e, HostError::KernelDied | HostError::Timeout(_));
            if !retry || attempt == START_ATTEMPTS {
                break Err(e.into());
            }
            tracing::warn!(kernel = %kernel.id, "kernel failed to start ({e}); launching again");
            attempt += 1;
            if let Err(e) = self.relaunch(kernel).await {
                break Err(e);
            }
        };
        kernel.booting.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => {
                let session = kernel.session();
                // The kernel answered, so it is idle unless a client's
                // request has made it busy since: the statuses for the
                // startup `kernel_info_request` are not tracked.
                {
                    let mut state = kernel.execution_state.lock().unwrap();
                    if matches!(state.as_str(), "starting" | "restarting") {
                        *state = "idle".into();
                    }
                }
                // A death while `booting` was set was not reported.
                if !session.is_alive() {
                    kernel.remove_dead(&self.kernels);
                }
                Ok(())
            }
            Err(e) => {
                tracing::error!(kernel = %kernel.id, "kernel failed to start: {e}");
                kernel.remove_dead(&self.kernels);
                Err(e)
            }
        }
    }

    /// Launch a new session for `kernel` and pump it to the clients.
    async fn relaunch(self: &Arc<Self>, kernel: &Arc<RunningKernel>) -> Result<()> {
        let (session, launched) = self
            .launch(&kernel.spec, kernel.cwd.clone(), &kernel.id)
            .await?;
        *kernel.session.write().unwrap() = session.clone();
        *kernel.launched.lock().unwrap() = Some(launched);
        // Requests to the old session will never be answered by this one.
        kernel.unanswered.lock().unwrap().clear();
        kernel.asked_to_exit.store(false, Ordering::SeqCst);
        kernel.pump(&session, Arc::downgrade(self));
        Ok(())
    }

    /// 404 when `kernel` has left the map, as it can while a caller waits
    /// for its lifecycle lock.
    fn ensure_listed(&self, kernel: &Arc<RunningKernel>) -> Result<()> {
        match self.kernels.read().unwrap().get(&kernel.id) {
            Some(k) if Arc::ptr_eq(k, kernel) => Ok(()),
            _ => Err(ApiError::not_found(format!("kernel {}", kernel.id))),
        }
    }

    async fn launch(
        &self,
        spec: &Kernelspec,
        cwd: Option<PathBuf>,
        id: &str,
    ) -> Result<(KernelSession, LaunchedKernel)> {
        let launched = self
            .provisioner
            .launch(
                spec,
                LaunchOptions {
                    cwd,
                    env: vec![],
                    kernel_id: Some(id.to_string()),
                    quiet: self.config.quiet,
                    // A server runs many kernels on one stderr.
                    prefix_output: true,
                },
            )
            .await?;
        let transport = self.transports.connect(&launched).await?;
        let session = KernelSession::start(transport, Some(&launched));
        Ok((session, launched))
    }
}

fn spec_model(s: &Kernelspec, base_url: &str) -> KernelspecModel {
    KernelspecModel {
        name: s.name.clone(),
        spec: KernelspecSpec {
            argv: s.argv.clone(),
            display_name: s.display_name.clone(),
            language: s.language.clone(),
            interrupt_mode: match s.interrupt_mode {
                ganymede_kernel_host::InterruptMode::Signal => "signal".into(),
                ganymede_kernel_host::InterruptMode::Message => "message".into(),
            },
            env: s.env.clone(),
            metadata: s.metadata.clone(),
            kernel_protocol_version: s
                .extra
                .get("kernel_protocol_version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        resources: spec_resources(s, base_url),
    }
}

/// Links to the files `jupyter_server` advertises from a kernelspec's
/// directory: `logo-*` keyed by file stem, plus `kernel.js` and
/// `kernel.css`. Served at `kernelspecs/{name}/{file}`.
fn spec_resources(s: &Kernelspec, base_url: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let Ok(entries) = std::fs::read_dir(&s.resource_dir) else {
        return out;
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    files.sort();
    for f in files {
        let key = if f.starts_with("logo-") {
            f.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&f)
        } else if f == "kernel.js" || f == "kernel.css" {
            &f
        } else {
            continue;
        };
        out.insert(
            key.to_string(),
            format!("{base_url}kernelspecs/{}/{f}", s.name).into(),
        );
    }
    out
}

/// Whether an iopub `status` should change the kernel's
/// `execution_state`. Statuses for these requests do not, so an interrupt
/// or a client's `kernel_info_request` mid-cell does not show the kernel
/// as idle. The list is `jupyter_server`'s `untracked_message_types`.
fn tracks_state(status: &Message) -> bool {
    const UNTRACKED: &[&str] = &[
        "comm_info_request",
        "comm_info_reply",
        "kernel_info_request",
        "kernel_info_reply",
        "shutdown_request",
        "shutdown_reply",
        "interrupt_request",
        "interrupt_reply",
        "debug_request",
        "debug_reply",
        "history_request",
        "history_reply",
        "usage_request",
        "usage_reply",
    ];
    let parent_type = status
        .parent_header
        .as_ref()
        .map(|h| h.msg_type.as_str())
        .unwrap_or_default();
    !UNTRACKED.contains(&parent_type)
}
