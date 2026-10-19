//! `ganymede lab` and `ganymede ui` with no subcommand: `ganymede serve`
//! for one person at a workstation, as `jupyter lab` is. `lab` requires an
//! installed JupyterLab and prints the URL of `/lab`; `ui` serves
//! ganymede-ui at `/`. Both open the URL in the browser and move to the
//! next port when 8888 is taken.

use anyhow::Result;
use ganymede_server::config::Config;
use ganymede_server::serve::{OnListening, ServeArgs, Startup};

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

/// Serve `cfg`, built from the [`StartArgs`]' `serve` flags; `lab`
/// requires JupyterLab and points the URL at it.
pub async fn run(cfg: Config, no_browser: bool, lab: bool) -> Result<()> {
    let on_listening: Option<OnListening> = if no_browser {
        None
    } else {
        Some(Box::new(|url: &str| open_browser(url)))
    };
    let startup = Startup {
        port_retries: PORT_RETRIES,
        require_lab: lab,
        on_listening,
        ..Default::default()
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
