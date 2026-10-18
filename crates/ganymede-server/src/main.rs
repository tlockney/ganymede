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
    let serve = Cli::parse().serve;
    ganymede_server::serve::init_logging(
        &ganymede_server::serve::log_filter(tracing::Level::INFO),
        Some(&serve),
    )?;
    let cfg = serve.into_config()?;
    ganymede_server::serve::run(cfg).await
}
