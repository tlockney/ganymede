//! `ganymede lab` and `ganymede ui` with no subcommand: `ganymede serve`
//! for one person at a workstation, as `jupyter lab` is. `lab` requires an
//! installed JupyterLab and prints the URL of `/lab`; `ui` serves
//! ganymede-ui at `/`. Both open the URL in the browser and move to the
//! next port when 8888 is taken.

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use ganymede_server::config::Config;
use ganymede_server::serve::{OnOpenPage, ServeArgs, Startup};

/// Ports tried after the first, as `jupyter_server`'s default
/// `port_retries`.
const PORT_RETRIES: u16 = 50;

#[derive(clap::Args)]
pub struct StartArgs {
    /// Print the URL without opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    /// Open the URL itself, token included, instead of the redirect page
    /// in the runtime dir: for snap or Flatpak browsers, which can't read
    /// it. `jupyter_server`'s `use_redirect_file = False`.
    #[arg(long)]
    pub no_redirect_file: bool,
    #[command(flatten)]
    pub serve: ServeArgs,
}

/// Serve `cfg`, built from the [`StartArgs`]' `serve` flags; `lab`
/// requires JupyterLab and points the URL at it.
pub async fn run(cfg: Config, args: Browser, lab: bool) -> Result<()> {
    let on_open_page: Option<OnOpenPage> = if args.no_browser {
        None
    } else {
        let redirect = !args.no_redirect_file;
        Some(Box::new(move |url: &str, page: Option<&Path>| {
            open_browser(url, page.filter(|_| redirect))
        }))
    };
    let startup = Startup {
        port_retries: PORT_RETRIES,
        require_lab: lab,
        on_open_page,
        ..Default::default()
    };
    ganymede_server::serve::run_with(cfg, startup).await
}

/// How `lab` and `ui` open the browser, from their flags.
pub struct Browser {
    pub no_browser: bool,
    pub no_redirect_file: bool,
}

/// Opens `page`, the `jpserver-<pid>-open.html` the server wrote, as
/// `jupyter_server` does, so the token stays out of the opener's argv;
/// `url` when there is none. A failure is only reported: the URL is on
/// stdout to open by hand.
fn open_browser(url: &str, page: Option<&Path>) {
    let target = browser_target(url, page);
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

/// What to hand the opener: the redirect page as a `file://` URL, so it
/// goes to the browser rather than to whatever opens `.html` files; else
/// the URL.
fn browser_target(url: &str, page: Option<&Path>) -> OsString {
    match page {
        Some(p) => file_url(p).into(),
        None => url.into(),
    }
}

/// `path`, absolute, as a `file://` URL, percent-encoding all but
/// unreserved characters and separators.
fn file_url(path: &Path) -> String {
    let mut path = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        path = path.replace('\\', "/");
    }
    let mut out = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_browser_gets_the_redirect_page_not_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("jpserver-1-open.html");
        let url = "http://127.0.0.1:8888/lab?token=secret";
        assert_eq!(browser_target(url, None), OsString::from(url));
        let target = browser_target(url, Some(&page));
        let target = target.to_str().unwrap();
        assert!(target.starts_with("file://"), "{target}");
        assert!(target.ends_with("/jpserver-1-open.html"), "{target}");
        assert!(!target.contains("secret"), "{target}");
        assert_eq!(
            file_url(Path::new("/home/me/a b/x.html")),
            "file:///home/me/a%20b/x.html"
        );
        #[cfg(windows)]
        assert_eq!(
            file_url(Path::new(r"C:\Users\me\x.html")),
            "file:///C:/Users/me/x.html"
        );
    }
}
