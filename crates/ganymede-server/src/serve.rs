//! Running the server: flags, config, startup, and shutdown. Shared by
//! `ganymede serve` and the `ganymede-server` binary.

use crate::config::Config;
use crate::AppState;
use anyhow::{Context, Result};
use ganymede_api::{KernelManager, KernelManagerConfig, SessionManager, TransportFactory};
use ganymede_contents::{FsContents, FsOptions, Notary};
use ganymede_kernel_host::{FsKernelspecs, KernelspecSource, Provisioner};
use ganymede_lsp::{LspConfig, LspManager};
use ganymede_terminals::{TerminalConfig, TerminalManager};
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Command line flags. Each overrides the config file's setting.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ServeArgs {
    /// TOML config file. Default: `ganymede.toml` in the Jupyter config
    /// dir (`$JUPYTER_CONFIG_DIR`, else `~/.jupyter`), if it exists.
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
}

/// The file name looked for in the Jupyter config dir when `--config` is
/// not given.
pub const DEFAULT_CONFIG_FILE: &str = "ganymede.toml";

impl ServeArgs {
    /// The config file, if any, with these flags applied on top. Without
    /// `--config`, [`DEFAULT_CONFIG_FILE`] in the Jupyter config dir is
    /// read when it exists.
    pub fn into_config(self) -> Result<Config> {
        let dir = ganymede_kernel_host::kernelspec::jupyter_config_dir();
        self.into_config_from(Some(&dir))
    }

    /// [`ServeArgs::into_config`], looking for the default file in
    /// `config_dir` (not at all when `None`).
    pub fn into_config_from(self, config_dir: Option<&std::path::Path>) -> Result<Config> {
        let cli = self;
        let path = cli.config.clone().or_else(|| {
            config_dir
                .map(|d| d.join(DEFAULT_CONFIG_FILE))
                .filter(|p| p.is_file())
        });
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
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::EnvFilter;
    let filter = match args.and_then(|a| a.log_level) {
        Some(level) => EnvFilter::new(log_filter(level)),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into()),
    };
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match args.and_then(|a| a.log_file.as_ref()) {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("log file {}", path.display()))?;
            builder
                .with_ansi(false)
                .with_writer(BoxMakeWriter::new(std::sync::Mutex::new(file)))
                .init();
        }
        None => builder.with_writer(std::io::stderr).init(),
    }
    Ok(())
}

/// Called with the printed URL once the port is bound.
pub type OnListening = Box<dyn FnOnce(&str) + Send>;

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

/// `gateway_url`'s kernels, refreshing its kernelspecs every minute.
#[cfg(feature = "gateway")]
fn gateway(cfg: &Config) -> Result<Option<Kernels>> {
    let Some(url) = cfg.gateway_url.as_deref().filter(|u| !u.is_empty()) else {
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
    if cfg.gateway_url.as_deref().is_some_and(|u| !u.is_empty()) {
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

/// Serve until Ctrl-C, then shut down every kernel. Prints the URL, with
/// its token, as the only line on stdout, and writes `jpserver-<pid>.json`
/// in the runtime dir for as long as it serves.
pub async fn run(cfg: Config) -> Result<()> {
    run_with(cfg, Startup::default()).await
}

/// [`run`], adjusted by `startup`.
pub async fn run_with(cfg: Config, startup: Startup) -> Result<()> {
    let token = match cfg.token.clone() {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t),
        None => Some(uuid::Uuid::new_v4().simple().to_string()),
    };
    let root_dir = cfg
        .root_dir
        .canonicalize()
        .with_context(|| format!("root dir {}", cfg.root_dir.display()))?;

    let base = cfg.base_url.trim_end_matches('/').to_string();
    let base_url = format!("{base}/");
    let config_dir = cfg
        .config_dir
        .clone()
        .unwrap_or_else(ganymede_kernel_host::kernelspec::jupyter_config_dir);
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
        max_body_size: usize::try_from(cfg.max_body_size).unwrap_or(usize::MAX),
        base_url: base_url.clone(),
        config_dir,
        static_dir: if lab.is_some() { None } else { static_dir },
        lab: lab.map(Arc::new),
        terminals: terminals.clone(),
        lsp: lsp.clone(),
    };

    let mut app = crate::build_router(state);
    if !base.is_empty() {
        app = axum::Router::new().nest(&base, app);
    }
    let app = app.layer(
        tower_http::trace::TraceLayer::new_for_http().make_span_with(
            |req: &axum::http::Request<axum::body::Body>| {
                tracing::debug_span!(
                    "request",
                    method = %req.method(),
                    uri = %crate::auth::scrub_uri(req.uri()),
                    version = ?req.version(),
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
    }
    // Written before the URL is printed, so whoever reads the URL finds
    // them. An embedding app has no use for them, and on iOS no runtime dir.
    let _info_files = (!embedded).then(|| {
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

    serve_until(listener, app, stop, DRAIN_TIMEOUT).await?;
    if let Some(t) = &terminals {
        t.shutdown_all();
    }
    if let Some(l) = &lsp {
        l.shutdown_all();
    }
    kernels.shutdown_all().await;
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
    use super::{serve_until, ServeArgs};
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
        let cfg = ServeArgs::default().into_config_from(None).unwrap();
        assert_eq!((cfg.ip.as_str(), cfg.port), ("127.0.0.1", 8888));
        assert_eq!(cfg.token, None);
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
        let (dir, _path) = config_file("port = 9001\n");
        let cfg = ServeArgs::default()
            .into_config_from(Some(dir.path()))
            .unwrap();
        assert_eq!(cfg.port, 9001);

        // --config wins over the default file.
        let (_other, path) = config_file("port = 9002\n");
        let cfg = ServeArgs {
            config: Some(path),
            ..Default::default()
        }
        .into_config_from(Some(dir.path()))
        .unwrap();
        assert_eq!(cfg.port, 9002);

        // No file there is the default config.
        let empty = tempfile::tempdir().unwrap();
        let cfg = ServeArgs::default()
            .into_config_from(Some(empty.path()))
            .unwrap();
        assert_eq!(cfg.port, 8888);
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
