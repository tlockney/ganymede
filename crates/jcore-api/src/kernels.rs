//! Running kernels by id, and the models for them.

use crate::{
    now_iso, ApiError, KernelModel, KernelspecModel, KernelspecSpec, KernelspecsModel, Result,
};
use async_trait::async_trait;
use jcore_kernel_host::protocol::{Channel, Message};
use jcore_kernel_host::{
    zmq::ZmqTransport, KernelSession, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// How a launched kernel gets its transport. ZMQ in the server; an
/// in-process pair in an app or a test.
#[async_trait]
pub trait TransportFactory: Send + Sync {
    async fn connect(
        &self,
        launched: &LaunchedKernel,
    ) -> jcore_kernel_host::Result<KernelTransport>;
}

pub struct ZmqTransportFactory;

#[async_trait]
impl TransportFactory for ZmqTransportFactory {
    async fn connect(
        &self,
        launched: &LaunchedKernel,
    ) -> jcore_kernel_host::Result<KernelTransport> {
        ZmqTransport::connect(&launched.connection).await
    }
}

/// A fixed list of kernelspecs. The iPad profile's source, and the test
/// double.
pub struct StaticKernelspecs(pub Vec<Kernelspec>);

impl KernelspecSource for StaticKernelspecs {
    fn list(&self) -> jcore_kernel_host::Result<Vec<Kernelspec>> {
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
    /// Shut down kernels idle this long with no connected clients.
    pub cull_idle: Option<Duration>,
    /// Working directories for kernels are resolved under this root.
    pub root_dir: PathBuf,
}

impl Default for KernelManagerConfig {
    fn default() -> Self {
        KernelManagerConfig {
            default_kernel: None,
            startup_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(5),
            cull_idle: None,
            root_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

/// One kernel the manager owns. The `hub` fans every message out to
/// clients and outlives restarts, so a client's subscription survives
/// the session being replaced.
pub struct RunningKernel {
    pub id: String,
    pub name: String,
    pub spec: Kernelspec,
    pub started: String,
    session: RwLock<KernelSession>,
    launched: Mutex<Option<LaunchedKernel>>,
    hub: broadcast::Sender<(Channel, Message)>,
    /// False once the manager has shut the kernel down. Clients hold an
    /// `Arc` to this struct, so the hub alone can never close; they watch
    /// this instead.
    alive: watch::Sender<bool>,
    last_activity: Mutex<String>,
    execution_state: Mutex<String>,
    connections: AtomicUsize,
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
            connections: self.connections.load(Ordering::SeqCst),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(Channel, Message)> {
        self.hub.subscribe()
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }

    pub fn watch_alive(&self) -> watch::Receiver<bool> {
        self.alive.subscribe()
    }

    pub(crate) fn add_connection(&self) {
        self.connections.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn remove_connection(&self) {
        self.connections.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = now_iso();
    }

    fn set_state(&self, state: &str) {
        *self.execution_state.lock().unwrap() = state.to_string();
    }

    /// Pump one session's traffic into the hub, tracking activity.
    fn pump(self: &Arc<Self>, session: &KernelSession) {
        let mut rx = session.subscribe();
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((channel, msg)) => {
                        me.touch();
                        if channel == Channel::IoPub && msg.msg_type() == "status" {
                            if let Some(s) =
                                msg.content.get("execution_state").and_then(|v| v.as_str())
                            {
                                me.set_state(s);
                            }
                        }
                        let _ = me.hub.send((channel, msg));
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(kernel = %me.id, "hub pump lagged by {n} messages")
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            if !me.session().is_alive() {
                me.set_state("dead");
            }
        });
    }

    /// A message the server itself injects, such as `status: restarting`.
    fn announce(&self, msg_type: &str, content: serde_json::Value) {
        let msg = Message::new(msg_type, "server", "server", &content);
        let _ = self.hub.send((Channel::IoPub, msg));
    }
}

pub struct KernelManager {
    specs: Arc<dyn KernelspecSource>,
    provisioner: Arc<dyn Provisioner>,
    transports: Arc<dyn TransportFactory>,
    kernels: RwLock<HashMap<String, Arc<RunningKernel>>>,
    pub config: KernelManagerConfig,
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
            provisioner,
            transports,
            kernels: RwLock::new(HashMap::new()),
            config,
            started: now_iso(),
        }
    }

    // ----- kernelspecs -------------------------------------------------

    pub fn kernelspecs(&self) -> Result<KernelspecsModel> {
        let specs = self.specs.list()?;
        let default = self
            .config
            .default_kernel
            .clone()
            .or_else(|| specs.first().map(|s| s.name.clone()))
            .unwrap_or_default();
        Ok(KernelspecsModel {
            default,
            kernelspecs: specs
                .into_iter()
                .map(|s| (s.name.clone(), spec_model(&s)))
                .collect(),
        })
    }

    pub fn kernelspec(&self, name: &str) -> Result<KernelspecModel> {
        Ok(spec_model(&self.specs.get(name)?))
    }

    // ----- kernels -----------------------------------------------------

    pub fn list(&self) -> Vec<KernelModel> {
        let mut v: Vec<_> = self
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
        self.kernels
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("kernel {id}")))
    }

    pub fn model(&self, id: &str) -> Result<KernelModel> {
        Ok(self.get(id)?.model())
    }

    /// Launch a kernel. `path` is the notebook's API path; its directory
    /// becomes the kernel's working directory.
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
        // The kernel runs in the notebook's directory, or the contents root
        // when no path is given, as it does under jupyter_server.
        let cwd = Some(match path {
            Some(p) => {
                let p = p.trim_start_matches('/');
                let dir = std::path::Path::new(p)
                    .parent()
                    .unwrap_or(std::path::Path::new(""));
                self.config.root_dir.join(dir)
            }
            None => self.config.root_dir.clone(),
        });
        let (session, launched) = self.launch(&spec, cwd, &id).await?;
        let (hub, _) = broadcast::channel(4096);
        let kernel = Arc::new(RunningKernel {
            id: id.clone(),
            name: spec.name.clone(),
            spec,
            started: now_iso(),
            session: RwLock::new(session.clone()),
            launched: Mutex::new(Some(launched)),
            hub,
            alive: watch::channel(true).0,
            last_activity: Mutex::new(now_iso()),
            execution_state: Mutex::new("starting".into()),
            connections: AtomicUsize::new(0),
        });
        kernel.pump(&session);
        self.kernels
            .write()
            .unwrap()
            .insert(id.clone(), kernel.clone());
        // The nudge runs after insertion so clients can connect while the
        // kernel starts, as they do against jupyter_server.
        match session.nudge(self.config.startup_timeout).await {
            Ok(_) => {
                kernel.set_state(session.execution_state_name());
                Ok(kernel.model())
            }
            Err(e) => {
                tracing::error!(kernel = %id, "kernel failed to start: {e}");
                let _ = self.shutdown(&id).await;
                Err(e.into())
            }
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
                },
            )
            .await?;
        let transport = self.transports.connect(&launched).await?;
        let session = KernelSession::start(transport, Some(&launched));
        Ok((session, launched))
    }

    pub async fn shutdown(&self, id: &str) -> Result<()> {
        let kernel = self
            .kernels
            .write()
            .unwrap()
            .remove(id)
            .ok_or_else(|| ApiError::not_found(format!("kernel {id}")))?;
        let session = kernel.session();
        session.shutdown(false, self.config.shutdown_grace).await?;
        kernel.set_state("dead");
        kernel.alive.send_replace(false);
        kernel.launched.lock().unwrap().take();
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> Result<()> {
        let kernel = self.get(id)?;
        kernel.session().interrupt().await?;
        kernel.touch();
        Ok(())
    }

    /// Same id, same hub, new process. Clients see `status: restarting`
    /// as they do from `jupyter_server`.
    pub async fn restart(&self, id: &str) -> Result<KernelModel> {
        let kernel = self.get(id)?;
        kernel.announce(
            "status",
            serde_json::json!({"execution_state": "restarting"}),
        );
        kernel.set_state("restarting");
        let old = kernel.session();
        let _ = old.shutdown(true, self.config.shutdown_grace).await;
        let (session, launched) = self.launch(&kernel.spec, None, id).await?;
        *kernel.session.write().unwrap() = session.clone();
        *kernel.launched.lock().unwrap() = Some(launched);
        kernel.pump(&session);
        session.nudge(self.config.startup_timeout).await?;
        kernel.set_state(session.execution_state_name());
        kernel.touch();
        Ok(kernel.model())
    }

    /// Shut down kernels idle longer than the configured limit with no
    /// clients connected. Returns the ids culled.
    pub async fn cull_idle(&self) -> Vec<String> {
        let Some(limit) = self.config.cull_idle else {
            return vec![];
        };
        let now = chrono::Utc::now();
        let candidates: Vec<String> = self
            .kernels
            .read()
            .unwrap()
            .values()
            .filter(|k| {
                k.connections.load(Ordering::SeqCst) == 0
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

    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = self.kernels.read().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.shutdown(&id).await;
        }
    }

    pub fn connections(&self) -> usize {
        self.kernels
            .read()
            .unwrap()
            .values()
            .map(|k| k.connections.load(Ordering::SeqCst))
            .sum()
    }

    pub fn last_activity(&self) -> String {
        self.kernels
            .read()
            .unwrap()
            .values()
            .map(|k| k.last_activity.lock().unwrap().clone())
            .max()
            .unwrap_or_else(|| self.started.clone())
    }
}

fn spec_model(s: &Kernelspec) -> KernelspecModel {
    KernelspecModel {
        name: s.name.clone(),
        spec: KernelspecSpec {
            argv: s.argv.clone(),
            display_name: s.display_name.clone(),
            language: s.language.clone(),
            interrupt_mode: match s.interrupt_mode {
                jcore_kernel_host::InterruptMode::Signal => "signal".into(),
                jcore_kernel_host::InterruptMode::Message => "message".into(),
            },
            env: s.env.clone(),
            metadata: s.metadata.clone(),
        },
        resources: Default::default(),
    }
}

trait StateName {
    fn execution_state_name(&self) -> &'static str;
}

impl StateName for KernelSession {
    fn execution_state_name(&self) -> &'static str {
        use jcore_kernel_host::protocol::content::ExecutionState::*;
        match self.execution_state() {
            Starting => "starting",
            Busy => "busy",
            Idle => "idle",
        }
    }
}
