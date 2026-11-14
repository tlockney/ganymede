//! Running the server: flags, config, startup, and shutdown. Shared by
//! `ganymede serve`, `ganymede mcp`, and the apps (`ganymede-ffi`).

use crate::config::Config;
use crate::AppState;
use anyhow::{Context, Result};
use ganymede_api::{KernelManager, KernelManagerConfig, SessionManager, TransportFactory};
use ganymede_contents::{FsContents, FsOptions, Notary};
use ganymede_kernel_host::{FsKernelspecs, KernelspecSource, Provisioner, RegisteredKernels};
use ganymede_lsp::{LspConfig, LspManager};
use ganymede_terminals::{TerminalConfig, TerminalManager};
use std::future::IntoFuture;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Command line flags. Each overrides the config file's setting.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ServeArgs {
    /// TOML config file. Default: `ganymede.toml` in Ganymede's config
    /// dir (`$XDG_CONFIG_HOME/ganymede`, else `~/.config/ganymede`;
    /// `%APPDATA%\ganymede` on Windows), if it exists.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Log level: trace, debug, info, warn or error (`WARNING` and
    /// `CRITICAL` as in Jupyter). Overrides -v and `RUST_LOG`.
    #[arg(long, value_name = "LEVEL", value_parser = parse_log_level)]
    pub log_level: Option<tracing::Level>,
    /// Append the log to this file instead of writing it to stderr.
    #[arg(long, value_name = "FILE")]
    pub log_file: Option<PathBuf>,
    /// Address to listen on. Default 127.0.0.1; a non-loopback address
    /// also turns on --allow-remote-access.
    #[arg(long)]
    pub ip: Option<String>,
    /// 0 picks a free port; the URL printed on stdout has it.
    #[arg(long)]
    pub port: Option<u16>,
    /// Auth token. Pass an empty string to disable auth.
    #[arg(long)]
    pub token: Option<String>,
    /// Directory served as the contents root. Default: the current
    /// directory.
    #[arg(long)]
    pub root_dir: Option<PathBuf>,
    /// Directory with the notebook-signing key and trusted signatures;
    /// see `notary_dir` in the config file.
    #[arg(long)]
    pub notary_dir: Option<PathBuf>,
    /// Static assets to serve; JupyterLab's `share/jupyter/lab/static`
    /// serves it at `/lab`. Defaults to the newest installed with
    /// `ganymede lab install`.
    #[arg(long)]
    pub static_dir: Option<PathBuf>,
    /// Installed JupyterLab version to serve when --static-dir is not given.
    #[arg(long, value_name = "VERSION")]
    pub lab_version: Option<String>,
    /// Extra directory of prebuilt JupyterLab extensions, searched before
    /// those from `ganymede lab extension install`. Repeatable.
    #[arg(long)]
    pub labextensions_path: Vec<PathBuf>,
    /// Frontend state directory; see `config_dir` in the config file.
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
    /// Extra kernelspec roots (directories holding `kernels/`), searched
    /// before the Jupyter data dirs. Repeatable.
    #[arg(long, value_name = "DIR")]
    pub kernelspec_root: Vec<PathBuf>,
    /// Kernelspec reported as the default, which new notebooks start.
    #[arg(long, value_name = "NAME")]
    pub default_kernel: Option<String>,
    /// Shut down kernels idle this many seconds with no clients
    /// connected. 0 (the default) never does.
    #[arg(long, value_name = "SECS")]
    pub cull_idle_secs: Option<u64>,
    /// Restart a kernel that crashes, up to this many times in a row,
    /// before leaving it dead. Default 5; 0 never restarts.
    #[arg(long, value_name = "N")]
    pub kernel_restart_limit: Option<u32>,
    /// Run kernels on this Jupyter server (`http(s)://host[:port][/base]`)
    /// instead of locally.
    #[arg(long, value_name = "URL", env = "JUPYTER_GATEWAY_URL")]
    pub gateway_url: Option<String>,
    /// Token for --gateway-url.
    #[arg(
        long,
        value_name = "TOKEN",
        env = "JUPYTER_GATEWAY_AUTH_TOKEN",
        hide_env_values = true
    )]
    pub gateway_token: Option<String>,
    /// Prefix for a reverse proxy, e.g. `/jupyter`.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Keep checkpoints here instead of `.ipynb_checkpoints` in the root.
    #[arg(long)]
    pub checkpoint_dir: Option<PathBuf>,
    /// Key file for signing login cookies; see `cookie_secret_file` in the
    /// config file.
    #[arg(long)]
    pub cookie_secret_file: Option<PathBuf>,
    /// An origin allowed to make requests besides the server's own.
    /// Repeatable.
    #[arg(long, value_name = "ORIGIN")]
    pub allow_origin: Vec<String>,
    /// Let pages on the --allow-origin origins use the login cookie, by
    /// sending `Access-Control-Allow-Credentials: true`. Ignored with `*`.
    #[arg(long)]
    pub allow_credentials: bool,
    /// Accept requests whose `Host` is not a loopback address or a name in
    /// `local_hostnames`. On by default when --ip is not loopback.
    #[arg(long)]
    pub allow_remote_access: bool,
    /// Serve the MCP server for agents at `/mcp`, behind the token; see
    /// `ganymede mcp`.
    #[arg(long)]
    pub mcp: bool,
}

/// The file name looked for in [`ganymede_config_dir`] when `--config`
/// is not given.
pub const DEFAULT_CONFIG_FILE: &str = "ganymede.toml";

/// Ganymede's config dir, which holds [`DEFAULT_CONFIG_FILE`]:
/// `$GANYMEDE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/ganymede`, else
/// `~/.config/ganymede`, on Linux and macOS; `$GANYMEDE_CONFIG_DIR`, else
/// `%APPDATA%\ganymede`, on Windows.
pub fn ganymede_config_dir() -> Option<PathBuf> {
    let platform = if cfg!(windows) {
        dirs::config_dir()
    } else {
        None
    };
    config_dir_from(
        std::env::var_os("GANYMEDE_CONFIG_DIR"),
        std::env::var_os("XDG_CONFIG_HOME"),
        dirs::home_dir(),
        platform,
    )
}

/// [`ganymede_config_dir`] from its inputs. `platform` is the OS's config
/// dir where it isn't XDG's (Windows), else `None`.
fn config_dir_from(
    ganymede: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    platform: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(d) = ganymede.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if let Some(p) = platform {
        return Some(p.join("ganymede"));
    }
    // The XDG spec ignores a relative value.
    match xdg.map(PathBuf::from) {
        Some(x) if x.is_absolute() => Some(x.join("ganymede")),
        _ => home.map(|h| h.join(".config").join("ganymede")),
    }
}

/// The default config file, as [`find_default_config`] finds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DefaultConfig {
    /// The file to read, if any.
    pub path: Option<PathBuf>,
    /// Said once when the file is, or would have been, the old one in the
    /// Jupyter config dir.
    pub warning: Option<String>,
}

/// The default config file: [`DEFAULT_CONFIG_FILE`] in `dir` when it
/// exists, else in `legacy_dir` (the Jupyter config dir, where earlier
/// releases read it), with a warning to move it.
pub fn find_default_config(dir: Option<&Path>, legacy_dir: Option<&Path>) -> DefaultConfig {
    let new = dir.map(|d| d.join(DEFAULT_CONFIG_FILE));
    let old = legacy_dir
        .map(|d| d.join(DEFAULT_CONFIG_FILE))
        .filter(|p| p.is_file() && !same_file(Some(p), new.as_deref()));
    let shown = |p: &Option<PathBuf>| {
        p.as_deref()
            .map_or("Ganymede's config dir".to_string(), |p| {
                p.display().to_string()
            })
    };
    match (new.clone().filter(|p| p.is_file()), old) {
        (Some(path), Some(old)) => DefaultConfig {
            path: Some(path),
            warning: Some(format!(
                "ignoring {}: the config file is now {}",
                old.display(),
                shown(&new)
            )),
        },
        (Some(path), None) => DefaultConfig {
            path: Some(path),
            warning: None,
        },
        (None, Some(old)) => DefaultConfig {
            warning: Some(format!(
                "reading {}, from the Jupyter config dir; move it to {}, where Ganymede looks first",
                old.display(),
                shown(&new)
            )),
            path: Some(old),
        },
        (None, None) => DefaultConfig::default(),
    }
}

/// Whether `a` and `b` name one file, through symlinks too.
fn same_file(a: Option<&Path>, b: Option<&Path>) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match (a, b) {
        (Some(a), Some(b)) => canon(a) == canon(b),
        _ => false,
    }
}

/// [`find_default_config`] in [`ganymede_config_dir`], then the Jupyter
/// config dir.
pub fn default_config() -> DefaultConfig {
    find_default_config(
        ganymede_config_dir().as_deref(),
        Some(&ganymede_kernel_host::kernelspec::jupyter_config_dir()),
    )
}

impl ServeArgs {
    /// The config file, if any, with these flags applied on top. Without
    /// `--config`, [`default_config`] is read when there is one.
    pub fn into_config(self) -> Result<Config> {
        let default = if self.config.is_some() {
            None
        } else {
            let d = default_config();
            if let Some(w) = &d.warning {
                tracing::warn!("{w}");
            }
            d.path
        };
        self.into_config_with(default)
    }

    /// [`ServeArgs::into_config`], reading `default` when `--config` is
    /// not given.
    pub fn into_config_with(self, default: Option<PathBuf>) -> Result<Config> {
        let cli = self;
        let path = cli.config.clone().or(default);
        let mut cfg = match &path {
            Some(p) => {
                tracing::info!("reading config from {}", p.display());
                Config::from_toml(
                    &std::fs::read_to_string(p)
                        .with_context(|| format!("reading {}", p.display()))?,
                )
                .with_context(|| format!("config file {}", p.display()))?
            }
            None => Config::default(),
        };
        if let Some(v) = cli.ip {
            cfg.ip = v;
        }
        if let Some(v) = cli.port {
            cfg.port = v;
        }
        if let Some(v) = cli.token {
            cfg.token = Some(v);
        }
        if let Some(v) = cli.root_dir {
            cfg.root_dir = v;
        }
        if let Some(v) = cli.notary_dir {
            cfg.notary_dir = Some(v);
        }
        if let Some(v) = cli.static_dir {
            cfg.static_dir = Some(v);
        }
        if let Some(v) = cli.lab_version {
            cfg.lab_version = Some(v);
        }
        if let Some(v) = cli.default_kernel {
            cfg.default_kernel = Some(v);
        }
        if let Some(v) = cli.cull_idle_secs {
            cfg.cull_idle_secs = v;
        }
        if let Some(v) = cli.kernel_restart_limit {
            cfg.kernel_restart_limit = v;
        }
        if let Some(v) = cli.gateway_url {
            cfg.gateway_url = Some(v);
        }
        if let Some(v) = cli.gateway_token {
            cfg.gateway_token = Some(v);
        }
        if let Some(v) = cli.config_dir {
            cfg.config_dir = Some(v);
        }
        if let Some(v) = cli.base_url {
            cfg.base_url = v;
        }
        if let Some(v) = cli.checkpoint_dir {
            cfg.checkpoint_dir = Some(v);
        }
        if let Some(v) = cli.cookie_secret_file {
            cfg.cookie_secret_file = Some(v);
        }
        cfg.allow_origins.extend(cli.allow_origin);
        if cli.allow_credentials {
            cfg.allow_credentials = true;
        }
        if cli.allow_remote_access {
            cfg.allow_remote_access = Some(true);
        }
        if cli.mcp {
            cfg.mcp_enabled = true;
        }
        cfg.kernelspec_roots.extend(cli.kernelspec_root);
        cfg.labextensions_paths.extend(cli.labextensions_path);

        Ok(cfg)
    }
}

/// `--log-level`: tracing's names and numbers, plus Python logging's
/// `WARNING` and `CRITICAL`, which Jupyter's `--log-level` takes.
fn parse_log_level(s: &str) -> std::result::Result<tracing::Level, String> {
    match s.to_ascii_lowercase().as_str() {
        "warning" => Ok(tracing::Level::WARN),
        "critical" => Ok(tracing::Level::ERROR),
        l => l
            .parse()
            .map_err(|_| format!("{s:?} is not one of trace, debug, info, warn, error")),
    }
}

/// The log filter for a server at `level`: kernel transport noise off,
/// and HTTP internals kept at info when debugging.
pub fn log_filter(level: tracing::Level) -> String {
    if level >= tracing::Level::DEBUG {
        format!("{level},zeromq=error,hyper=info,h2=info").to_lowercase()
    } else {
        format!("{level},zeromq=error").to_lowercase()
    }
}

/// Install the global tracing subscriber. The filter is `--log-level`'s
/// when given, else `RUST_LOG`, else `default_filter`; the log goes to
/// `--log-file` (appended, no colors) when given, else stderr. `args` is
/// `None` for commands that do not serve.
pub fn init_logging(default_filter: &str, args: Option<&ServeArgs>) -> Result<()> {
    init_logging_with(default_filter, args, None)
}

/// A layer that sees events beside the log output, such as a crash
/// reporter's. It gets every event its own filter lets through, not only
/// those the log filter does.
pub type ExtraLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// [`init_logging`], plus `extra` beside the log output.
pub fn init_logging_with(
    default_filter: &str,
    args: Option<&ServeArgs>,
    extra: Option<ExtraLayer>,
) -> Result<()> {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;
    let filter = match args.and_then(|a| a.log_level) {
        Some(level) => EnvFilter::new(log_filter(level)),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into()),
    };
    let (writer, ansi) = match args.and_then(|a| a.log_file.as_ref()) {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("log file {}", path.display()))?;
            (BoxMakeWriter::new(std::sync::Mutex::new(file)), false)
        }
        None => (BoxMakeWriter::new(std::io::stderr), true),
    };
    let log = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(ansi)
        .with_filter(filter);
    tracing_subscriber::registry().with(extra).with(log).init();
    Ok(())
}

/// Called with the printed URL once the port is bound.
pub type OnListening = Box<dyn FnOnce(&str) + Send>;

/// Called with the printed URL and the `jpserver-<pid>-open.html` this
/// server wrote (`None` when it wrote none) once the port is bound.
pub type OnOpenPage = Box<dyn FnOnce(&str, Option<&Path>) + Send>;

/// What `ganymede lab` asks of [`run_with`] beyond `ganymede serve`.
#[derive(Default)]
pub struct Startup {
    /// Ports after the configured one to try while it is taken, like
    /// `jupyter_server`'s `port_retries`.
    pub port_retries: u16,
    /// Fail instead of serving only the API when there is no JupyterLab,
    /// and print the URL of `/lab` rather than of the root.
    pub require_lab: bool,
    pub on_listening: Option<OnListening>,
    /// Like `on_listening`, also given the redirect page, so a browser can
    /// be opened without the token in its arguments.
    pub on_open_page: Option<OnOpenPage>,
    /// Stop on a message here instead of on Ctrl-C or SIGTERM, and print
    /// nothing to stdout: how an app embedding the server runs it.
    pub stop: Option<tokio::sync::mpsc::Receiver<()>>,
    /// Where kernelspecs come from, instead of the config's kernelspec
    /// dirs: an app listing the kernels it runs itself.
    pub kernelspecs: Option<Arc<dyn KernelspecSource>>,
    /// How kernels start, instead of as subprocesses.
    pub provisioner: Option<Arc<dyn Provisioner>>,
    /// How the server talks to kernels, instead of over ZeroMQ.
    pub transports: Option<Arc<dyn TransportFactory>>,
    /// Kernels the app runs itself and registers (#266), listed after
    /// the others (the gateway's, or those the fields above give) and in
    /// place of one of the same name.
    pub registered: Option<Arc<RegisteredKernels>>,
}

#[cfg(feature = "subprocess")]
fn default_provisioner() -> Result<Arc<dyn Provisioner>> {
    Ok(Arc::new(
        ganymede_kernel_host::SubprocessProvisioner::default(),
    ))
}

#[cfg(not(feature = "subprocess"))]
fn default_provisioner() -> Result<Arc<dyn Provisioner>> {
    anyhow::bail!("no kernel provisioner: built without `subprocess` and none given")
}

/// A kernelspec source, provisioner and transport factory.
type Kernels = (
    Arc<dyn KernelspecSource>,
    Arc<dyn Provisioner>,
    Arc<dyn TransportFactory>,
);

/// Whether `cfg` runs kernels on another server (`gateway_url` is set).
pub fn gateway_configured(cfg: &Config) -> bool {
    cfg.gateway_url.as_deref().is_some_and(|u| !u.is_empty())
}

/// `gateway_url`'s kernels, refreshing its kernelspecs every minute.
#[cfg(feature = "gateway")]
fn gateway(cfg: &Config) -> Result<Option<Kernels>> {
    let Some(url) = cfg
        .gateway_url
        .as_deref()
        .filter(|_| gateway_configured(cfg))
    else {
        return Ok(None);
    };
    let g = Arc::new(ganymede_gateway::Gateway::new(
        url,
        cfg.gateway_token.clone(),
    )?);
    g.refresh_every(Duration::from_secs(60));
    tracing::info!("kernels run on the gateway at {url}");
    Ok(Some((g.clone(), g.clone(), g)))
}

#[cfg(not(feature = "gateway"))]
fn gateway(cfg: &Config) -> Result<Option<Kernels>> {
    if gateway_configured(cfg) {
        anyhow::bail!("gateway_url is set, but this build has no `gateway` feature");
    }
    Ok(None)
}

#[cfg(feature = "zmq")]
fn default_transports() -> Result<Arc<dyn TransportFactory>> {
    Ok(Arc::new(ganymede_api::ZmqTransportFactory))
}

#[cfg(not(feature = "zmq"))]
fn default_transports() -> Result<Arc<dyn TransportFactory>> {
    anyhow::bail!("no kernel transport factory: built without `zmq` and none given")
}

/// The notary for `dir`, else for the Jupyter data dir. When neither can
/// hold a key, notebooks are signed for this process only.
pub fn notary(dir: Option<PathBuf>) -> Notary {
    let dir = dir.or_else(ganymede_kernel_host::kernelspec::jupyter_user_data_dir);
    let opened = match &dir {
        Some(d) => Notary::open(d).map_err(|e| format!("{}: {e}", d.display())),
        None => Err("no Jupyter data dir".to_string()),
    };
    opened.unwrap_or_else(|e| {
        tracing::warn!("notebook trust is per-process: {e}");
        Notary::ephemeral().expect("the OS provides random bytes")
    })
}

/// Document rooms' file ids for `root`, kept in the Jupyter data dir's
/// `ganymede_file_ids.db` when there is one.
pub fn file_ids(root: &std::path::Path) -> ganymede_contents::FileIds {
    match ganymede_kernel_host::kernelspec::jupyter_user_data_dir() {
        Some(dir) => ganymede_contents::FileIds::open(&dir.join("ganymede_file_ids.db"), root),
        None => ganymede_contents::FileIds::in_memory(),
    }
}

/// The host API without a listener (`docs/host-api.md`): how
/// `ganymede execute` answers its kernels' calls. API paths are relative
/// to `root_dir`; the grants are a notebook's own, kept with the file ids
/// for `root_dir` as `ganymede grant` keeps them, over `cfg.host`. Kernels
/// it starts are only the runs its kernels ask for (`notebooks.run`),
/// from `cfg.kernelspec_roots`.
pub struct LocalHost {
    api: Arc<crate::host::HostApi>,
    responder: Arc<dyn ganymede_host::Responder>,
    state: AppState,
}

impl LocalHost {
    pub fn new(cfg: &Config, root_dir: &Path) -> Result<Self> {
        let root_dir = root_dir
            .canonicalize()
            .with_context(|| format!("root dir {}", root_dir.display()))?;
        let kernels = Arc::new(KernelManager::new(
            Arc::new(FsKernelspecs::discover(cfg.kernelspec_roots.clone())),
            default_provisioner()?,
            default_transports()?,
            KernelManagerConfig {
                startup_timeout: Duration::from_secs(cfg.startup_timeout_secs),
                quiet: cfg.quiet,
                restart_limit: 0,
                root_dir: root_dir.clone(),
                ..Default::default()
            },
        ));
        let state = AppState {
            sessions: Arc::new(SessionManager::new(kernels.clone())),
            kernels: kernels.clone(),
            contents: Arc::new(FsContents::new(&root_dir, FsOptions::default())?),
            token: None,
            cookie_secret: Arc::from(&[][..]),
            allow_origins: Vec::new(),
            allow_credentials: false,
            allow_remote_access: false,
            local_hostnames: Vec::new(),
            trust_xheaders: false,
            tailnet_allow: Arc::from([]),
            tailnet_whois: None,
            max_body_size: 0,
            base_url: "/".into(),
            config_dir: cfg
                .config_dir
                .clone()
                .unwrap_or_else(ganymede_kernel_host::kernelspec::jupyter_config_dir),
            lab: None,
            static_dir: None,
            static_headers: Arc::new([]),
            frame_ancestors: crate::frame_ancestors_directive("'self'")?,
            // The same file ids as a server on this root and `ganymede
            // grant`, so a notebook's own grant applies here too.
            collab: Arc::new(crate::collab::Collab::new(
                file_ids(&root_dir),
                Some(root_dir),
            )),
            terminals: None,
            lsp: None,
            mcp: None,
            metrics: Default::default(),
            host: Arc::new(cfg.host.clone()),
        };
        let api = Arc::new(crate::host::HostApi::new(state.clone(), &cfg.host));
        let responder: Arc<dyn ganymede_host::Responder> = api.clone();
        kernels.set_host(&responder, env!("CARGO_PKG_VERSION"));
        Ok(LocalHost {
            api,
            responder,
            state,
        })
    }

    /// Answer `session`'s calls as the kernel of `notebook`, an API path,
    /// for as long as the link is kept.
    pub fn attach(
        &self,
        session: &ganymede_kernel_host::KernelSession,
        notebook: Option<String>,
    ) -> LocalHostLink {
        let kernel_id = uuid::Uuid::new_v4().simple().to_string();
        self.api.pin(&kernel_id, notebook);
        let link = ganymede_host::attach(
            session,
            ganymede_host::Caller {
                kernel_id: kernel_id.clone(),
            },
            self.responder.clone(),
            env!("CARGO_PKG_VERSION"),
        );
        LocalHostLink {
            api: self.api.clone(),
            kernel_id,
            _link: link,
        }
    }

    /// Stop any run kernels still going.
    pub async fn shutdown(&self) {
        self.state.kernels.shutdown_all().await;
    }
}

/// One kernel's host comm from [`LocalHost::attach`]. Dropping it stops
/// answering and forgets the kernel.
pub struct LocalHostLink {
    api: Arc<crate::host::HostApi>,
    kernel_id: String,
    _link: ganymede_host::HostLink,
}

impl Drop for LocalHostLink {
    fn drop(&mut self) {
        self.api.unpin(&self.kernel_id);
    }
}

/// Serve until Ctrl-C, then shut down every kernel. Prints the URL, with
/// its token, as the only line on stdout, and writes `jpserver-<pid>.json`
/// in the runtime dir for as long as it serves.
pub async fn run(cfg: Config) -> Result<()> {
    run_with(cfg, Startup::default()).await
}

/// `--base-url` without its trailing slash and with a leading one, as
/// `jupyter_server` adds it: `jupyter` serves under `/jupyter/`, and `/`
/// or an empty value is the root (an empty string here).
fn normalize_base_url(base_url: &str) -> String {
    let base = base_url.trim_matches('/');
    if base.is_empty() {
        String::new()
    } else {
        format!("/{base}")
    }
}

/// [`run`], adjusted by `startup`.
pub async fn run_with(mut cfg: Config, startup: Startup) -> Result<()> {
    cfg.tailscale.validate().map_err(anyhow::Error::msg)?;
    if !cfg.tailscale.allow.is_empty() && cfg.token.as_deref() == Some("") {
        anyhow::bail!(
            "[tailscale] allow has no effect with auth off (token = \"\"): set a token, or \
             leave allow empty"
        );
    }
    if !cfg.tailscale.allow.is_empty() && cfg.allow_remote_access() {
        anyhow::bail!(
            "[tailscale] allow needs the Host check: leave allow_remote_access off \
             (add the tailnet name to local_hostnames instead)"
        );
    }
    // `whois` needs the server on exactly its tailnet address: on any
    // other, or on all of them, addresses in the tailnet ranges could come
    // from elsewhere, and the server couldn't tell its own address (which
    // local processes connect from) from a peer's. Clients may name the
    // server by that address, so the Host check accepts it.
    let whois_own = if cfg.tailscale.whois && !cfg.tailscale.allow.is_empty() {
        let ip = cfg
            .ip
            .parse::<std::net::IpAddr>()
            .ok()
            .filter(|ip| crate::tailscale::is_tailnet_addr(*ip));
        let Some(ip) = ip else {
            anyhow::bail!(
                "[tailscale] whois needs ip set to this machine's tailnet address \
                 (`tailscale ip -4`), not {:?}",
                cfg.ip
            );
        };
        cfg.local_hostnames.push(ip.to_string());
        Some(ip)
    } else {
        None
    };
    let token = match cfg.token.clone() {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t),
        None => Some(uuid::Uuid::new_v4().simple().to_string()),
    };
    let root_dir = cfg
        .root_dir
        .canonicalize()
        .with_context(|| format!("root dir {}", cfg.root_dir.display()))?;

    let base = normalize_base_url(&cfg.base_url);
    let base_url = format!("{base}/");
    let config_dir = cfg
        .config_dir
        .clone()
        .unwrap_or_else(ganymede_kernel_host::kernelspec::jupyter_config_dir);
    let static_headers = crate::static_headers::StaticHeaders::parse(&cfg.static_headers)?;
    let frame_ancestors = crate::frame_ancestors_directive(&cfg.frame_ancestors)?;
    let static_dir = match &cfg.static_dir {
        Some(d) => Some(d.clone()),
        None => installed_lab(cfg.lab_version.as_deref())?,
    };
    let lab = static_dir.as_ref().and_then(|dir| {
        let mut ext_dirs = cfg.labextensions_paths.clone();
        ext_dirs.extend(crate::lab::extensions_dir());
        ext_dirs.extend(
            ganymede_kernel_host::kernelspec::jupyter_data_dirs()
                .into_iter()
                .map(|d| d.join("labextensions")),
        );
        crate::lab::Lab::detect(dir, &ext_dirs, &config_dir, &root_dir)
    });
    match (&static_dir, &lab) {
        (_, Some(_)) => {}
        (Some(d), None) if startup.require_lab => {
            anyhow::bail!("{} is not a JupyterLab static dir", d.display())
        }
        (None, None) if startup.require_lab => {
            anyhow::bail!("no JupyterLab installed; run `ganymede lab install` first")
        }
        (None, None) => tracing::info!(
            "no JupyterLab installed; serving the API only (`ganymede lab install` adds it)"
        ),
        (Some(_), None) => {}
    }
    if (lab.is_some() || static_dir.is_none()) && !static_headers.is_empty() {
        tracing::warn!("static_headers applies to a static_dir other than JupyterLab; ignored");
    }
    if let Some(lab) = &lab {
        tracing::info!(
            "serving {} {} from {} with {} prebuilt extension(s)",
            lab.app_name,
            lab.app_version,
            lab.static_dir.display(),
            lab.extensions.len()
        );
    }

    let (specs, provisioner, transports) = match gateway(&cfg)? {
        // An app's own kernels win over the config's gateway.
        Some((k, p, t)) => (
            startup.kernelspecs.unwrap_or(k),
            startup.provisioner.unwrap_or(p),
            startup.transports.unwrap_or(t),
        ),
        None => (
            startup
                .kernelspecs
                .unwrap_or_else(|| Arc::new(FsKernelspecs::discover(cfg.kernelspec_roots.clone()))),
            match startup.provisioner {
                Some(p) => p,
                None => default_provisioner()?,
            },
            match startup.transports {
                Some(t) => t,
                None => default_transports()?,
            },
        ),
    };
    let (specs, provisioner, transports) = match startup.registered {
        Some(r) => {
            r.set_fallback(specs, provisioner, transports);
            (
                r.clone() as Arc<dyn KernelspecSource>,
                r.clone() as Arc<dyn Provisioner>,
                r as Arc<dyn TransportFactory>,
            )
        }
        None => (specs, provisioner, transports),
    };
    let kernels = Arc::new(KernelManager::new(
        specs,
        provisioner,
        transports,
        KernelManagerConfig {
            default_kernel: cfg.default_kernel.clone(),
            startup_timeout: Duration::from_secs(cfg.startup_timeout_secs),
            quiet: cfg.quiet,
            cull_idle: (cfg.cull_idle_secs > 0).then(|| Duration::from_secs(cfg.cull_idle_secs)),
            restart_limit: cfg.kernel_restart_limit,
            root_dir: root_dir.clone(),
            base_url: base_url.clone(),
            ..Default::default()
        },
    ));
    let sessions = Arc::new(SessionManager::new(kernels.clone()));
    let contents = Arc::new(FsContents::new(
        &root_dir,
        FsOptions {
            checkpoint_dir: cfg.checkpoint_dir.clone(),
            notary: Some(Arc::new(notary(cfg.notary_dir.clone()))),
            ..Default::default()
        },
    )?);
    let cookie_secret_file = cfg.cookie_secret_file.clone().unwrap_or_else(|| {
        ganymede_kernel_host::kernelspec::jupyter_runtime_dir().join("jupyter_cookie_secret")
    });
    let terminals = (cfg.terminals_enabled && TerminalManager::supported()).then(|| {
        Arc::new(TerminalManager::new(TerminalConfig {
            shell: cfg.terminal_shell.clone(),
            root_dir: root_dir.clone(),
        }))
    });
    let lsp = LspManager::supported()
        .then(|| {
            LspManager::new(LspConfig {
                servers: cfg.language_servers.clone(),
                root_dir: root_dir.clone(),
            })
        })
        .filter(|m| !m.is_empty())
        .map(Arc::new);
    if !cfg.language_servers.is_empty() && !LspManager::supported() {
        tracing::warn!("language servers are configured but cannot run on this platform");
    }
    // An embedding app has no runtime dir on iOS, so its rooms don't lock.
    let mut collab = crate::collab::Collab::new(file_ids(&root_dir), Some(root_dir.clone()));
    if startup.stop.is_none() {
        collab = collab.with_room_locks(
            ganymede_kernel_host::kernelspec::jupyter_runtime_dir().join("ganymede-rooms"),
        );
    }
    let state_collab = Arc::new(collab);
    let state = AppState {
        kernels: kernels.clone(),
        sessions,
        contents,
        token: token.clone(),
        cookie_secret: crate::auth::load_cookie_secret(&cookie_secret_file).into(),
        allow_origins: cfg.allow_origins.clone(),
        allow_credentials: cfg.allow_credentials,
        allow_remote_access: cfg.allow_remote_access(),
        local_hostnames: cfg.local_hostnames.clone(),
        trust_xheaders: cfg.trust_xheaders,
        tailnet_allow: cfg.tailscale.allow.clone().into(),
        tailnet_whois: whois_own
            .map(|own| Arc::new(crate::tailscale::WhoIs::new(cfg.tailscale.cli.clone(), own))),
        max_body_size: usize::try_from(cfg.max_body_size).unwrap_or(usize::MAX),
        base_url: base_url.clone(),
        config_dir,
        static_dir: if lab.is_some() { None } else { static_dir },
        static_headers: static_headers.into(),
        frame_ancestors: frame_ancestors.clone(),
        lab: lab.map(Arc::new),
        collab: state_collab.clone(),
        terminals: terminals.clone(),
        lsp: lsp.clone(),
        mcp: cfg.mcp_enabled.then(|| {
            Arc::new(
                crate::mcp::McpState::new(crate::mcp::McpOptions {
                    read_only: cfg.mcp_read_only,
                    no_execute: cfg.mcp_no_execute,
                    images: cfg.mcp_images,
                    outputs_dir: None,
                })
                .with_token(cfg.mcp_token.clone()),
            )
        }),
        metrics: Default::default(),
        host: Arc::new(cfg.host.clone()),
    };

    // Kept here for as long as the server runs: the manager holds it
    // weakly, since it holds the state that holds the manager.
    let host: Arc<dyn ganymede_host::Responder> =
        Arc::new(crate::host::HostApi::new(state.clone(), &cfg.host));
    kernels.set_host(&host, env!("CARGO_PKG_VERSION"));
    let mut app = crate::build_router(state);
    if !base.is_empty() {
        // Paths outside the prefix get the same frame-ancestors.
        app = axum::Router::new()
            .nest(&base, app)
            .layer(axum::middleware::from_fn_with_state(
                frame_ancestors,
                crate::frame_ancestors,
            ));
    }
    let app = app.layer(
        tower_http::trace::TraceLayer::new_for_http().make_span_with(
            |req: &axum::http::Request<axum::body::Body>| {
                tracing::debug_span!(
                    "request",
                    method = %req.method(),
                    uri = %crate::auth::scrub_uri(req.uri()),
                    version = ?req.version(),
                    who = tracing::field::Empty,
                )
            },
        ),
    );

    if cfg.cull_idle_secs > 0 {
        let k = kernels.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                k.cull_idle().await;
            }
        });
    }

    let embedded = startup.stop.is_some();
    let stop = match startup.stop {
        Some(stop) => stop,
        None => stop_signals()?,
    };
    let listener = bind(&cfg.ip, cfg.port, startup.port_retries).await?;
    let addr = listener.local_addr()?;
    let page = if startup.require_lab { "lab" } else { "" };
    let url = match &token {
        Some(t) => format!("http://{addr}{base_url}{page}?token={t}"),
        None => format!("http://{addr}{base_url}{page}"),
    };
    tracing::info!("serving {} at {url}", root_dir.display());
    if token.is_none() {
        tracing::warn!("auth disabled: anyone who can reach this port can run code");
    } else if !cfg.tailscale.allow.is_empty() {
        let how = if cfg.tailscale.whois {
            "`tailscale serve` or `tailscale whois`"
        } else {
            "`tailscale serve`"
        };
        tracing::info!(
            "also letting in, without the token, requests that {how} names as: {}",
            cfg.tailscale.allow.join(", ")
        );
        if !cfg.tailscale.whois && !addr.ip().is_loopback() {
            tracing::warn!(
                "[tailscale] allow is set but the server listens on {addr}: tailnet logins are \
                 believed only from loopback peers, that is through `tailscale serve`"
            );
        }
    }
    // Written before the URL is printed, so whoever reads the URL finds
    // them. An embedding app has no use for them, and on iOS no runtime dir.
    let info_files = (!embedded).then(|| {
        crate::server_info::ServerInfoFiles::write(
            &ganymede_kernel_host::kernelspec::jupyter_runtime_dir(),
            &crate::server_info::ServerInfo {
                ip: &cfg.ip,
                addr,
                base_url: &base_url,
                token: token.as_deref(),
                root_dir: &root_dir,
                page,
            },
        )
    });
    if !embedded {
        println!("{url}");
    }
    if let Some(f) = startup.on_listening {
        f(&url);
    }
    if let Some(f) = startup.on_open_page {
        f(&url, info_files.as_ref().and_then(|f| f.open_html()));
    }

    serve_until(listener, app, stop, DRAIN_TIMEOUT).await?;
    state_collab.flush_all().await;
    if let Some(t) = &terminals {
        t.shutdown_all();
    }
    if let Some(l) = &lsp {
        l.shutdown_all();
    }
    kernels.shutdown_all().await;
    drop(host);
    Ok(())
}

/// How long open requests get to finish after the first stop signal.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Serves `app` until the first message on `stop`, then lets open requests
/// finish until `drain` passes or a second message arrives. axum's
/// graceful shutdown alone waits for every connection without limit, so
/// one unfinished request kept Ctrl-C from stopping the server (#125).
async fn serve_until(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    mut stop: tokio::sync::mpsc::Receiver<()>,
    drain: Duration,
) -> std::io::Result<()> {
    let stopping = Arc::new(tokio::sync::Notify::new());
    let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown({
            let stopping = stopping.clone();
            async move { stopping.notified().await }
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        r = &mut server => return r,
        Some(()) = stop.recv() => {}
    }
    tracing::info!("shutting down kernels");
    stopping.notify_one();
    tokio::select! {
        r = server => r,
        () = tokio::time::sleep(drain) => {
            tracing::info!("closing connections still open after {drain:?}");
            Ok(())
        }
        Some(()) = stop.recv() => {
            tracing::info!("stopping without waiting for open connections");
            Ok(())
        }
    }
}

/// A listener on `port`, or on the first free one of the `retries` after
/// it. Port 0 lets the OS choose.
async fn bind(ip: &str, port: u16, retries: u16) -> Result<tokio::net::TcpListener> {
    let mut port = port;
    let mut left = if port == 0 { 0 } else { retries };
    loop {
        match tokio::net::TcpListener::bind((ip, port)).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && left > 0 => {
                tracing::info!("port {port} is in use, trying {}", port + 1);
            }
            Err(e) => return Err(e).with_context(|| format!("binding {ip}:{port}")),
        }
        let Some(next) = port.checked_add(1) else {
            anyhow::bail!("no free port after {port}");
        };
        port = next;
        left -= 1;
    }
}

/// The static dir of the JupyterLab installed by `ganymede lab install`:
/// `version` if given, which must be installed, else the newest. `None`
/// when none is installed, so only the API is served.
fn installed_lab(version: Option<&str>) -> Result<Option<PathBuf>> {
    let installs = crate::lab::installs_dir()
        .map(|d| crate::lab::installed(&d))
        .unwrap_or_default();
    match version {
        Some(v) => match installs.iter().find(|i| i.version == v) {
            Some(i) => Ok(Some(i.static_dir())),
            None => anyhow::bail!(
                "JupyterLab {v} is not installed; run `ganymede lab install --version {v}`"
            ),
        },
        None => Ok(installs.first().map(crate::lab::Installed::static_dir)),
    }
}

/// A message per Ctrl-C, or SIGTERM where there is one, so a service
/// manager's stop also shuts kernels down. The handlers are installed
/// once, before serving, so no signal falls between them.
fn stop_signals() -> Result<tokio::sync::mpsc::Receiver<()>> {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut int = signal(SignalKind::interrupt()).context("SIGINT handler")?;
        let mut term = signal(SignalKind::terminate()).context("SIGTERM handler")?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(()) = int.recv() => {}
                    Some(()) = term.recv() => {}
                    else => break,
                }
                if tx.send(()).await.is_err() {
                    break;
                }
            }
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            if tx.send(()).await.is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::{find_default_config, serve_until, DefaultConfig, ServeArgs, DEFAULT_CONFIG_FILE};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn config_file(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ganymede.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn no_file_and_no_flags_is_the_default() {
        let cfg = ServeArgs::default().into_config_with(None).unwrap();
        assert_eq!((cfg.ip.as_str(), cfg.port), ("127.0.0.1", 8888));
        assert_eq!(cfg.token, None);
    }

    /// `--base-url jupyter` panicked at startup: the router was nested
    /// at a path without a leading slash.
    #[test]
    fn a_base_url_gets_a_leading_slash() {
        use super::normalize_base_url;
        for (given, want) in [
            ("jupyter", "/jupyter"),
            ("jupyter/", "/jupyter"),
            ("/jupyter/", "/jupyter"),
            ("/a/b", "/a/b"),
            ("/", ""),
            ("", ""),
        ] {
            assert_eq!(normalize_base_url(given), want, "{given:?}");
        }
    }

    #[test]
    fn flags_override_the_file_and_lists_extend_it() {
        let (_dir, path) = config_file(
            "port = 9000\ntoken = \"file\"\nbase_url = \"/a/\"\ncull_idle_secs = 30\n\
             allow_origins = [\"https://a\"]\nkernelspec_roots = [\"/k1\"]\n",
        );
        let cfg = ServeArgs {
            config: Some(path),
            port: Some(0),
            token: Some(String::new()),
            base_url: Some("/b/".into()),
            checkpoint_dir: Some("/cp".into()),
            cookie_secret_file: Some("/secret".into()),
            allow_origin: vec!["https://b".into()],
            kernelspec_root: vec!["/k2".into()],
            allow_remote_access: true,
            ..Default::default()
        }
        .into_config()
        .unwrap();
        assert_eq!(cfg.port, 0);
        assert_eq!(cfg.token.as_deref(), Some(""));
        assert_eq!(cfg.base_url, "/b/");
        assert_eq!(cfg.cull_idle_secs, 30);
        assert_eq!(cfg.checkpoint_dir, Some("/cp".into()));
        assert_eq!(cfg.cookie_secret_file, Some("/secret".into()));
        assert_eq!(cfg.allow_origins, ["https://a", "https://b"]);
        assert_eq!(
            cfg.kernelspec_roots,
            [PathBuf::from("/k1"), PathBuf::from("/k2")]
        );
        assert_eq!(cfg.allow_remote_access, Some(true));
    }

    #[test]
    fn the_default_file_is_read_from_the_config_dir() {
        let (dir, path) = config_file("port = 9001\n");
        let cfg = ServeArgs::default()
            .into_config_with(Some(path.clone()))
            .unwrap();
        assert_eq!(cfg.port, 9001);

        // --config wins over the default file.
        let (_other, other) = config_file("port = 9002\n");
        let cfg = ServeArgs {
            config: Some(other),
            ..Default::default()
        }
        .into_config_with(Some(path))
        .unwrap();
        assert_eq!(cfg.port, 9002);

        // No file is the default config.
        let cfg = ServeArgs::default().into_config_with(None).unwrap();
        assert_eq!(cfg.port, 8888);
        drop(dir);
    }

    #[test]
    fn the_default_file_falls_back_to_the_jupyter_config_dir() {
        let new = tempfile::tempdir().unwrap();
        let old = tempfile::tempdir().unwrap();
        let file = |d: &tempfile::TempDir| d.path().join(DEFAULT_CONFIG_FILE);
        let find = || find_default_config(Some(new.path()), Some(old.path()));

        // Neither: no file, no warning.
        assert_eq!(find(), DefaultConfig::default());

        // Only the old one: read it, and say where it goes now.
        std::fs::write(file(&old), "").unwrap();
        let d = find();
        assert_eq!(d.path, Some(file(&old)));
        let w = d.warning.unwrap();
        assert!(w.contains(&file(&new).display().to_string()), "{w}");

        // Both: the new one, and the old one is named as ignored.
        std::fs::write(file(&new), "").unwrap();
        let d = find();
        assert_eq!(d.path, Some(file(&new)));
        assert!(d.warning.unwrap().starts_with("ignoring "));

        // Only the new one: no warning.
        std::fs::remove_file(file(&old)).unwrap();
        assert_eq!(
            find(),
            DefaultConfig {
                path: Some(file(&new)),
                warning: None
            }
        );

        // The same dir twice (JUPYTER_CONFIG_DIR pointed at it) is no
        // fallback.
        assert_eq!(
            find_default_config(Some(new.path()), Some(new.path())).warning,
            None
        );
    }

    #[test]
    fn the_config_dir_follows_ganymede_then_xdg_then_home() {
        use super::config_dir_from;
        let home = Some(PathBuf::from("/home/u"));
        let at = |g: Option<&str>, x: Option<&str>, p: Option<&str>| {
            config_dir_from(
                g.map(Into::into),
                x.map(Into::into),
                home.clone(),
                p.map(Into::into),
            )
        };
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(at(None, None, None), p("/home/u/.config/ganymede"));
        assert_eq!(at(None, Some("/x"), None), p("/x/ganymede"));
        // A relative or empty XDG_CONFIG_HOME is ignored, as the spec says.
        assert_eq!(at(None, Some("rel"), None), p("/home/u/.config/ganymede"));
        assert_eq!(at(None, Some(""), None), p("/home/u/.config/ganymede"));
        // GANYMEDE_CONFIG_DIR wins, unless empty.
        assert_eq!(at(Some("/g"), Some("/x"), Some("/w")), p("/g"));
        assert_eq!(at(Some(""), Some("/x"), None), p("/x/ganymede"));
        // Windows: %APPDATA%, not XDG.
        assert_eq!(at(None, Some("/x"), Some("/w")), p("/w/ganymede"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_jupyter_dir_is_one_file() {
        let new = tempfile::tempdir().unwrap();
        std::fs::write(new.path().join(DEFAULT_CONFIG_FILE), "").unwrap();
        let link = tempfile::tempdir().unwrap();
        let old = link.path().join("jupyter");
        std::os::unix::fs::symlink(new.path(), &old).unwrap();
        let d = find_default_config(Some(new.path()), Some(&old));
        assert_eq!(d.warning, None);
    }

    #[test]
    fn log_levels_take_jupyter_names() {
        use tracing::Level;
        for (s, level) in [
            ("DEBUG", Level::DEBUG),
            ("info", Level::INFO),
            ("WARN", Level::WARN),
            ("WARNING", Level::WARN),
            ("error", Level::ERROR),
            ("CRITICAL", Level::ERROR),
            ("trace", Level::TRACE),
        ] {
            assert_eq!(super::parse_log_level(s), Ok(level), "{s}");
        }
        assert!(super::parse_log_level("loud").is_err());
        assert_eq!(super::log_filter(Level::WARN), "warn,zeromq=error");
        assert!(super::log_filter(Level::DEBUG).starts_with("debug,"));
    }

    #[test]
    fn unknown_keys_in_the_file_fail() {
        let (_dir, path) = config_file("port = 0\ntokn = \"x\"\n");
        let err = ServeArgs {
            config: Some(path),
            ..Default::default()
        }
        .into_config()
        .unwrap_err();
        assert!(format!("{err:#}").contains("tokn"), "{err:#}");
    }

    #[test]
    fn a_missing_file_fails() {
        let err = ServeArgs {
            config: Some("/nonexistent/ganymede.toml".into()),
            ..Default::default()
        }
        .into_config()
        .unwrap_err();
        assert!(format!("{err:#}").contains("reading"), "{err:#}");
    }

    /// A server with one connection stuck mid-request, and the sender of
    /// its stop signals.
    async fn stuck_server(
        drain: Duration,
    ) -> (
        tokio::task::JoinHandle<std::io::Result<()>>,
        tokio::sync::mpsc::Sender<()>,
        tokio::net::TcpStream,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let server = tokio::spawn(serve_until(listener, app, rx, drain));
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        // Headers without the blank line that ends them.
        conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        (server, tx, conn)
    }

    /// Handlers see the peer address, which tailnet identity needs (#816).
    #[tokio::test]
    async fn requests_carry_the_peer_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(
                |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<
                    std::net::SocketAddr,
                >| async move { peer.ip().to_string() },
            ),
        );
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let server = tokio::spawn(serve_until(listener, app, rx, Duration::from_millis(100)));
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut conn, &mut resp)
            .await
            .unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.ends_with("127.0.0.1"), "{resp}");
        tx.send(()).await.unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_unfinished_request_does_not_hold_shutdown_past_the_drain() {
        let (server, tx, _conn) = stuck_server(Duration::from_millis(200)).await;
        tx.send(()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server stopped after the drain")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_second_signal_stops_without_waiting() {
        let (server, tx, _conn) = stuck_server(Duration::from_secs(3600)).await;
        tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!server.is_finished(), "still draining");
        tx.send(()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("server stopped on the second signal")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn idle_connections_close_at_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let server = tokio::spawn(serve_until(
            listener,
            axum::Router::new(),
            rx,
            Duration::from_secs(3600),
        ));
        let _conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("graceful shutdown closed the idle connection")
            .unwrap()
            .unwrap();
    }
}
