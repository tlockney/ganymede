use anyhow::{Context, Result};
use clap::Parser;
use jcore_api::{KernelManager, KernelManagerConfig, SessionManager, ZmqTransportFactory};
use jcore_contents::{FsContents, FsOptions};
use jcore_kernel_host::{FsKernelspecs, SubprocessProvisioner};
use jcore_server::config::Config;
use jcore_server::AppState;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "jcore-server", about = "Python-free Jupyter server")]
struct Cli {
    /// TOML config file.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    ip: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    /// Auth token. Pass an empty string to disable auth.
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    root_dir: Option<PathBuf>,
    #[arg(long)]
    static_dir: Option<PathBuf>,
    #[arg(long)]
    kernelspec_root: Vec<PathBuf>,
    #[arg(long)]
    default_kernel: Option<String>,
    #[arg(long)]
    cull_idle_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
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
    if let Some(v) = cli.static_dir {
        cfg.static_dir = Some(v);
    }
    if let Some(v) = cli.default_kernel {
        cfg.default_kernel = Some(v);
    }
    if let Some(v) = cli.cull_idle_secs {
        cfg.cull_idle_secs = v;
    }
    cfg.kernelspec_roots.extend(cli.kernelspec_root);

    let token = match cfg.token.clone() {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t),
        None => Some(uuid::Uuid::new_v4().simple().to_string()),
    };
    let root_dir = cfg
        .root_dir
        .canonicalize()
        .with_context(|| format!("root dir {}", cfg.root_dir.display()))?;

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
            ..Default::default()
        },
    ));
    let sessions = Arc::new(SessionManager::new(kernels.clone()));
    let contents = Arc::new(FsContents::new(
        &root_dir,
        FsOptions {
            checkpoint_dir: cfg.checkpoint_dir.clone(),
            ..Default::default()
        },
    )?);
    let state = AppState {
        kernels: kernels.clone(),
        sessions,
        contents,
        token: token.clone(),
        allow_origins: cfg.allow_origins.clone(),
    };

    let mut app = jcore_server::build_router(state);
    if let Some(dir) = &cfg.static_dir {
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir).append_index_html_on_directories(true),
        );
    }
    let base = cfg.base_url.trim_end_matches('/');
    if !base.is_empty() {
        app = axum::Router::new().nest(base, app);
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
        Some(t) => format!("http://{addr}{base}/?token={t}"),
        None => format!("http://{addr}{base}/"),
    };
    tracing::info!("serving {} at {url}", root_dir.display());
    if token.is_none() {
        tracing::warn!("auth disabled: anyone who can reach this port can run code");
    }
    println!("{url}");

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down kernels");
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    kernels.shutdown_all().await;
    Ok(())
}
