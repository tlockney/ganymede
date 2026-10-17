use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "ganymede-server", about = "Python-free Jupyter server")]
struct Cli {
    #[command(flatten)]
    serve: ganymede_server::serve::ServeArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cfg = Cli::parse().serve.into_config()?;
    ganymede_server::serve::run(cfg).await
}
