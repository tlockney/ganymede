//! `ganymede lab` with no subcommand: `ganymede serve` for one person at a
//! workstation, as `jupyter lab` is. It requires an installed JupyterLab,
//! prints the URL of `/lab`, opens it in the browser, and moves to the next
//! port when 8888 is taken.

use anyhow::Result;
use ganymede_server::serve::{OnListening, ServeArgs, Startup};
use std::path::PathBuf;

/// Ports tried after the first, as `jupyter_server`'s default
/// `port_retries`.
const PORT_RETRIES: u16 = 50;

#[derive(clap::Args)]
pub struct StartArgs {
    /// Print the URL without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    #[command(flatten)]
    pub serve: ServeArgs,
}

pub async fn run(args: StartArgs, kernelspec_roots: Vec<PathBuf>) -> Result<()> {
    let mut cfg = args.serve.into_config()?;
    cfg.kernelspec_roots.extend(kernelspec_roots);
    let on_listening: Option<OnListening> = if args.no_browser {
        None
    } else {
        Some(Box::new(|url: &str| open_browser(url)))
    };
    let startup = Startup {
        port_retries: PORT_RETRIES,
        require_lab: true,
        on_listening,
    };
    ganymede_server::serve::run_with(cfg, startup).await
}

/// Opens `url` with the platform's opener. A failure is only reported:
/// the URL is on stdout to open by hand.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Reaped off the server's threads; xdg-open can take a moment.
        Ok(mut child) => {
            std::thread::spawn(move || {
                if let Ok(status) = child.wait() {
                    if !status.success() && !cfg!(windows) {
                        tracing::warn!("{opener} failed ({status}); open the URL by hand");
                    }
                }
            });
        }
        Err(e) => tracing::warn!("cannot run {opener}: {e}; open the URL by hand"),
    }
}
