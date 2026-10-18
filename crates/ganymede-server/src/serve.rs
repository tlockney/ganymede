//! Running the server: flags, config, startup, and shutdown. Shared by
//! `ganymede serve` and the `ganymede-server` binary.

use crate::config::Config;
use crate::AppState;
use anyhow::{Context, Result};
use ganymede_api::{KernelManager, KernelManagerConfig, SessionManager, ZmqTransportFactory};
use ganymede_contents::{FsContents, FsOptions, Notary};
use ganymede_kernel_host::{FsKernelspecs, SubprocessProvisioner};
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
    #[arg(long)]
    pub ip: Option<String>,
    /// 0 picks a free port; the URL printed on stdout has it.
    #[arg(long)]
    pub port: Option<u16>,
    /// Auth token. Pass an empty string to disable auth.
    #[arg(long)]
    pub token: Option<String>,
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
    #[arg(long)]
    pub kernelspec_root: Vec<PathBuf>,
    #[arg(long)]
    pub default_kernel: Option<String>,
    #[arg(long)]
    pub cull_idle_secs: Option<u64>,
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
/// its token, as the only line on stdout.
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

    let specs = Arc::new(FsKernelspecs::discover(cfg.kernelspec_roots.clone()));
    let kernels = Arc::new(KernelManager::new(
        specs,
        Arc::new(SubprocessProvisioner::default()),
        Arc::new(ZmqTransportFactory),
        KernelManagerConfig {
            default_kernel: cfg.default_kernel.clone(),
            startup_timeout: Duration::from_secs(cfg.startup_timeout_secs),
            quiet: cfg.quiet,
            cull_idle: (cfg.cull_idle_secs > 0).then(|| Duration::from_secs(cfg.cull_idle_secs)),
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
    let state = AppState {
        kernels: kernels.clone(),
        sessions,
        contents,
        token: token.clone(),
        cookie_secret: crate::auth::load_cookie_secret(&cookie_secret_file).into(),
        allow_origins: cfg.allow_origins.clone(),
        allow_remote_access: cfg.allow_remote_access(),
        local_hostnames: cfg.local_hostnames.clone(),
        trust_xheaders: cfg.trust_xheaders,
        max_body_size: usize::try_from(cfg.max_body_size).unwrap_or(usize::MAX),
        base_url: base_url.clone(),
        config_dir,
        static_dir: if lab.is_some() { None } else { static_dir },
        lab: lab.map(Arc::new),
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
    println!("{url}");
    if let Some(f) = startup.on_listening {
        f(&url);
    }

    let shutdown = async {
        shutdown_signal().await;
        tracing::info!("shutting down kernels");
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    kernels.shutdown_all().await;
    Ok(())
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

/// Ctrl-C, or SIGTERM where there is one, so a service manager's stop
/// also shuts kernels down.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::ServeArgs;
    use std::path::PathBuf;

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
}
