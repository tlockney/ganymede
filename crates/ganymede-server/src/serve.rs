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
    /// TOML config file.
    #[arg(long)]
    pub config: Option<PathBuf>,
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
    /// Extra directory of prebuilt JupyterLab extensions. Repeatable.
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
}

impl ServeArgs {
    /// The config file, if any, with these flags applied on top.
    pub fn into_config(self) -> Result<Config> {
        let cli = self;
        let mut cfg = match &cli.config {
            Some(p) => Config::from_toml(
                &std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
            )?,
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
        cfg.kernelspec_roots.extend(cli.kernelspec_root);
        cfg.labextensions_paths.extend(cli.labextensions_path);

        Ok(cfg)
    }
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
        ext_dirs.extend(
            ganymede_kernel_host::kernelspec::jupyter_data_dirs()
                .into_iter()
                .map(|d| d.join("labextensions")),
        );
        crate::lab::Lab::detect(dir, &ext_dirs, &config_dir, &root_dir)
    });
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
        base_url: base_url.clone(),
        config_dir,
        static_dir: if lab.is_some() { None } else { static_dir },
        lab: lab.map(Arc::new),
    };

    let mut app = crate::build_router(state);
    if !base.is_empty() {
        app = axum::Router::new().nest(&base, app);
    }
    let app = app.layer(tower_http::trace::TraceLayer::new_for_http());

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

    let listener = tokio::net::TcpListener::bind((cfg.ip.as_str(), cfg.port)).await?;
    let addr = listener.local_addr()?;
    let url = match &token {
        Some(t) => format!("http://{addr}{base_url}?token={t}"),
        None => format!("http://{addr}{base_url}"),
    };
    tracing::info!("serving {} at {url}", root_dir.display());
    if token.is_none() {
        tracing::warn!("auth disabled: anyone who can reach this port can run code");
    }
    println!("{url}");

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
        None => {
            if installs.is_empty() {
                tracing::info!(
                    "no JupyterLab installed; serving the API only (`ganymede lab install` adds it)"
                );
            }
            Ok(installs.first().map(crate::lab::Installed::static_dir))
        }
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
