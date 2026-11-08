//! `ganymede lab` and `ganymede ui` with no subcommand: `ganymede serve`
//! for one person at a workstation, as `jupyter lab` is. `lab` requires an
//! installed JupyterLab and prints the URL of `/lab`; `ui` serves
//! ganymede-ui at `/`. Both open the URL in the browser and move to the
//! next port when 8888 is taken.

use std::ffi::OsString;
use std::path::Path;

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

/// Opens the server's `jpserver-<pid>-open.html`, as `jupyter_server`
/// does, so the token stays out of the opener's argv; `url` when that file
/// is missing. A failure is only reported: the URL is on stdout to open by
/// hand.
fn open_browser(url: &str) {
    let open_html = ganymede_engine::host::kernelspec::jupyter_runtime_dir()
        .join(format!("jpserver-{}-open.html", std::process::id()));
    let target = browser_target(url, &open_html);
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(opener)
        .arg(target)
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

/// What to hand the opener: the redirect page when the server wrote one,
/// else the URL.
fn browser_target(url: &str, open_html: &Path) -> OsString {
    if open_html.is_file() {
        open_html.as_os_str().to_owned()
    } else {
        url.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browser_gets_the_redirect_page_not_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("jpserver-1-open.html");
        let url = "http://127.0.0.1:8888/lab?token=secret";
        assert_eq!(browser_target(url, &page), OsString::from(url));
        std::fs::write(&page, "<html></html>").unwrap();
        assert_eq!(browser_target(url, &page), page.into_os_string());
    }
}
