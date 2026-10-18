//! Finding kernels and bringing them up.

use crate::kernel::Kernel;
#[cfg(feature = "zmq")]
use crate::kernel::Relaunch;
#[cfg(feature = "zmq")]
use crate::Error;
use crate::Result;
#[cfg(feature = "zmq")]
use ganymede_kernel_host::protocol::ConnectionInfo;
#[cfg(feature = "zmq")]
use ganymede_kernel_host::{
    attach,
    process::{find_connection_file, read_connection_file},
    zmq::ZmqTransport,
    LaunchOptions,
};
use ganymede_kernel_host::{
    FsKernelspecs, KernelSession, KernelTransport, Kernelspec, KernelspecSource, LaunchedKernel,
    Provisioner,
};
use ganymede_nbformat::Notebook;
#[cfg(feature = "zmq")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Where kernels come from. Cheap to clone.
#[derive(Clone)]
#[cfg_attr(not(feature = "zmq"), allow(dead_code))]
pub struct Engine {
    specs: Arc<dyn KernelspecSource>,
    provisioner: Option<Arc<dyn Provisioner>>,
    startup_timeout: Duration,
    quiet: bool,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

/// Per-launch settings.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// The kernel's working directory. Defaults to the host's.
    pub cwd: Option<PathBuf>,
    /// Added to the host's environment and the kernelspec's `env`.
    pub env: Vec<(String, String)>,
}

impl Engine {
    /// Kernelspecs from the standard Jupyter data directories, kernels as
    /// subprocesses over ZeroMQ.
    pub fn new() -> Self {
        Engine::builder().build()
    }

    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    pub fn kernelspecs(&self) -> Result<Vec<Kernelspec>> {
        Ok(self.specs.list()?)
    }

    /// Case-insensitive lookup.
    pub fn kernelspec(&self, name: &str) -> Result<Kernelspec> {
        Ok(self.specs.get(name)?)
    }

    pub fn kernelspec_source(&self) -> &Arc<dyn KernelspecSource> {
        &self.specs
    }

    /// Launch the named kernelspec and wait until it answers.
    #[cfg(feature = "zmq")]
    pub async fn start(&self, name: &str) -> Result<Kernel> {
        self.start_with(name, StartOptions::default()).await
    }

    #[cfg(feature = "zmq")]
    pub async fn start_with(&self, name: &str, opts: StartOptions) -> Result<Kernel> {
        let spec = self.kernelspec(name)?;
        let provisioner = self
            .provisioner
            .clone()
            .ok_or_else(|| Error::Usage("no provisioner: set one on the builder".into()))?;
        let relaunch = Relaunch {
            provisioner,
            spec: spec.clone(),
            opts: LaunchOptions {
                cwd: opts.cwd,
                env: opts.env,
                kernel_id: None,
                quiet: self.quiet,
            },
        };
        let (session, launched, info) = relaunch.start(self.startup_timeout).await?;
        Ok(Kernel::started(
            session,
            Some(launched),
            info,
            Some(spec),
            Some(relaunch),
            self.startup_timeout,
        ))
    }

    /// Use a running kernel by its connection file, as
    /// `jupyter console --existing` does. The engine does not own it:
    /// shutting down asks it to exit, and restart is unsupported. A bare
    /// name that is not a file here is looked up in the Jupyter runtime
    /// dir, and an empty path picks the newest kernel there (see
    /// [`find_connection_file`]).
    #[cfg(feature = "zmq")]
    pub async fn attach(&self, connection_file: &Path) -> Result<Kernel> {
        let path = find_connection_file(
            connection_file,
            &ganymede_kernel_host::kernelspec::jupyter_runtime_dir(),
        )?;
        let info = read_connection_file(&path)?;
        self.connect(info).await
    }

    #[cfg(feature = "zmq")]
    pub async fn connect(&self, info: ConnectionInfo) -> Result<Kernel> {
        let launched = attach(info);
        let transport = ZmqTransport::connect(&launched.connection).await?;
        let session = KernelSession::start(transport, Some(&launched));
        Kernel::ready(session, Some(launched), None, None, self.startup_timeout).await
    }

    /// Adopt a kernel the application drives itself, such as an
    /// in-process kernel on a device with no subprocesses or sockets.
    pub async fn adopt(&self, transport: KernelTransport) -> Result<Kernel> {
        let session = KernelSession::start(transport, None);
        Kernel::ready(session, None, None, None, self.startup_timeout).await
    }

    /// Adopt a kernel already launched by some other provisioner.
    pub async fn adopt_launched(
        &self,
        launched: LaunchedKernel,
        transport: KernelTransport,
    ) -> Result<Kernel> {
        let session = KernelSession::start(transport, Some(&launched));
        Kernel::ready(session, Some(launched), None, None, self.startup_timeout).await
    }

    /// Execute a notebook file on a fresh kernel and return the result,
    /// leaving the file as it was. The kernel is the notebook's
    /// `metadata.kernelspec.name` unless `kernel` is given; it runs in the
    /// notebook's directory.
    #[cfg(feature = "zmq")]
    pub async fn execute_notebook(
        &self,
        path: &Path,
        kernel: Option<&str>,
        opts: &ganymede_exec::Options,
    ) -> Result<(
        Notebook,
        std::result::Result<ganymede_exec::Report, ganymede_exec::Error>,
    )> {
        let mut nb = ganymede_nbformat::read_file(path)?;
        let name = match kernel {
            Some(k) => k.to_string(),
            None => notebook_kernel_name(&nb).ok_or_else(|| {
                Error::Usage(format!(
                    "{} has no metadata.kernelspec.name; name a kernel",
                    path.display()
                ))
            })?,
        };
        let cwd = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let kernel = self
            .start_with(&name, StartOptions { cwd, env: vec![] })
            .await?;
        let report = kernel.run_notebook(&mut nb, opts).await;
        kernel.shutdown().await?;
        Ok((nb, report))
    }
}

/// `metadata.kernelspec.name`, which is what clients start for a notebook.
pub fn notebook_kernel_name(nb: &Notebook) -> Option<String> {
    nb.metadata
        .get("kernelspec")
        .and_then(|k| k.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
}

#[derive(Default)]
pub struct EngineBuilder {
    roots: Vec<PathBuf>,
    specs: Option<Arc<dyn KernelspecSource>>,
    provisioner: Option<Arc<dyn Provisioner>>,
    startup_timeout: Option<Duration>,
    quiet: bool,
}

impl EngineBuilder {
    /// A directory holding `kernels/<name>/kernel.json`, searched before
    /// the Jupyter data directories. Repeatable.
    pub fn kernelspec_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.roots.push(root.into());
        self
    }

    /// Replace discovery entirely, e.g. with a fixed list on a device.
    pub fn kernelspecs(mut self, source: Arc<dyn KernelspecSource>) -> Self {
        self.specs = Some(source);
        self
    }

    pub fn provisioner(mut self, p: Arc<dyn Provisioner>) -> Self {
        self.provisioner = Some(p);
        self
    }

    /// How long a new kernel has to answer `kernel_info_request`. 60 s
    /// by default, as `jupyter_client`.
    pub fn startup_timeout(mut self, t: Duration) -> Self {
        self.startup_timeout = Some(t);
        self
    }

    /// Discard kernel processes' own stdout and stderr.
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn build(self) -> Engine {
        Engine {
            specs: self
                .specs
                .unwrap_or_else(|| Arc::new(FsKernelspecs::discover(self.roots))),
            provisioner: self.provisioner.or_else(default_provisioner),
            startup_timeout: self.startup_timeout.unwrap_or(Duration::from_secs(60)),
            quiet: self.quiet,
        }
    }
}

#[cfg(feature = "subprocess")]
fn default_provisioner() -> Option<Arc<dyn Provisioner>> {
    Some(Arc::new(
        ganymede_kernel_host::SubprocessProvisioner::default(),
    ))
}

#[cfg(not(feature = "subprocess"))]
fn default_provisioner() -> Option<Arc<dyn Provisioner>> {
    None
}
