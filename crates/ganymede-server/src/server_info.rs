//! The server info file, `jpserver-<pid>.json` in the Jupyter runtime dir,
//! and its `jpserver-<pid>-open.html`, as `jupyter_server` writes them.
//! `jupyter server list` and clients that look for a running server (such
//! as nb-cli's `nb connect`) read them; both hold the token, so they are
//! written readable by the owner only, and removed when the server stops.

use serde_json::{json, Value};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// What goes in the file. Built from the server's settings once it is
/// listening, so `port` is the bound one.
pub struct ServerInfo<'a> {
    /// The configured `--ip`.
    pub ip: &'a str,
    /// The address the listener got.
    pub addr: SocketAddr,
    /// Starts and ends with `/`.
    pub base_url: &'a str,
    /// `None` when auth is off; written as `""`, as `jupyter_server` does.
    pub token: Option<&'a str>,
    pub root_dir: &'a Path,
    /// The page the open file redirects to, under `base_url`: `lab` or
    /// empty.
    pub page: &'a str,
}

impl ServerInfo<'_> {
    /// The URL a client connects to: the base URL without a token, the
    /// `url` key. A wildcard `ip` becomes `localhost`, which a client on
    /// this machine can reach (`jupyter_server` puts the host name there).
    pub fn url(&self) -> String {
        let host = match self.ip {
            "" | "0.0.0.0" | "::" => "localhost".to_string(),
            ip if ip.contains(':') => format!("[{ip}]"),
            ip => ip.to_string(),
        };
        format!("http://{host}:{}{}", self.addr.port(), self.base_url)
    }

    /// `jupyter_server`'s `ServerApp.server_info()`, key for key, plus
    /// `ganymede_version` so a client can tell the two apart.
    pub fn to_json(&self) -> Value {
        json!({
            "url": self.url(),
            "hostname": if self.ip.is_empty() { "localhost" } else { self.ip },
            "port": self.addr.port(),
            "sock": "",
            "secure": false,
            "base_url": self.base_url,
            "token": self.token.unwrap_or(""),
            "root_dir": self.root_dir,
            "password": false,
            "pid": std::process::id(),
            "version": crate::routes::JUPYTER_SERVER_VERSION,
            "ganymede_version": env!("CARGO_PKG_VERSION"),
        })
    }

    /// The page that sends a browser to the server with the token, so the
    /// token need not be on a command line.
    fn open_html(&self) -> String {
        let mut url = format!("{}{}", self.url(), self.page);
        if let Some(t) = self.token {
            url.push_str("?token=");
            url.push_str(&form_urlencoded::byte_serialize(t.as_bytes()).collect::<String>());
        }
        let url = html_attr(&url);
        format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    \
             <meta http-equiv=\"refresh\" content=\"1;url={url}\" />\n    \
             <title>Opening Jupyter Application</title>\n</head>\n<body>\n\n<p>\n    \
             This page should redirect you to a Jupyter application. If it doesn't,\n    \
             <a href=\"{url}\">click here to go to Jupyter</a>.\n</p>\n\n</body>\n</html>\n"
        )
    }
}

fn html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The written files; dropping it removes them.
pub struct ServerInfoFiles {
    paths: Vec<PathBuf>,
}

impl ServerInfoFiles {
    /// Writes `jpserver-<pid>.json` and `jpserver-<pid>-open.html` in
    /// `runtime_dir`, creating it, and replacing files of the same name
    /// left by an earlier process that had this pid. A failure is logged
    /// and leaves the server running without the files, as in
    /// `jupyter_server`.
    pub fn write(runtime_dir: &Path, info: &ServerInfo<'_>) -> Self {
        let mut files = ServerInfoFiles { paths: Vec::new() };
        if let Err(e) = create_private_dir(runtime_dir) {
            tracing::warn!("cannot create {}: {e}", runtime_dir.display());
            return files;
        }
        let pid = std::process::id();
        let mut json = serde_json::to_string_pretty(&info.to_json()).expect("plain JSON");
        json.push('\n');
        for (name, text) in [
            (format!("jpserver-{pid}.json"), json),
            (format!("jpserver-{pid}-open.html"), info.open_html()),
        ] {
            let path = runtime_dir.join(name);
            match write_private(&path, text.as_bytes()) {
                Ok(()) => files.paths.push(path),
                Err(e) => tracing::warn!("cannot write {}: {e}", path.display()),
            }
        }
        if let Some(p) = files.paths.first() {
            tracing::debug!("wrote {}", p.display());
        }
        files
    }

    /// The files written, the JSON first.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl Drop for ServerInfoFiles {
    fn drop(&mut self) {
        for p in &self.paths {
            match std::fs::remove_file(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("cannot remove {}: {e}", p.display()),
            }
        }
    }
}

/// Creates `dir` and its parents; the last level is made owner-only on
/// Unix, as `jupyter_core` makes the runtime dir.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    match b.create(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        r => r,
    }
}

/// Writes `bytes` to `path` through a temporary file in the same dir, so a
/// reader never sees half a file. The temporary file is owner-only (0600
/// on Unix) and the rename keeps that.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".jpserver-")
        .tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info<'a>(root: &'a Path, token: Option<&'a str>) -> ServerInfo<'a> {
        ServerInfo {
            ip: "127.0.0.1",
            addr: "127.0.0.1:8890".parse().unwrap(),
            base_url: "/jupyter/",
            token,
            root_dir: root,
            page: "lab",
        }
    }

    #[test]
    fn the_json_has_jupyter_servers_keys() {
        let root = Path::new("/notebooks");
        let v = info(root, Some("abc")).to_json();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "base_url",
                "ganymede_version",
                "hostname",
                "password",
                "pid",
                "port",
                "root_dir",
                "secure",
                "sock",
                "token",
                "url",
                "version"
            ]
        );
        assert_eq!(v["url"], "http://127.0.0.1:8890/jupyter/");
        assert_eq!(v["port"], 8890);
        assert_eq!(v["token"], "abc");
        assert_eq!(v["root_dir"], "/notebooks");
        assert_eq!(v["pid"], std::process::id());
        assert_eq!(info(root, None).to_json()["token"], "");
    }

    #[test]
    fn wildcard_and_ipv6_addresses_make_connectable_urls() {
        let root = Path::new("/");
        let mut i = info(root, None);
        i.ip = "0.0.0.0";
        assert_eq!(i.url(), "http://localhost:8890/jupyter/");
        i.ip = "::1";
        assert_eq!(i.url(), "http://[::1]:8890/jupyter/");
    }

    #[test]
    fn files_are_private_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let pid = std::process::id();
        let json = runtime.join(format!("jpserver-{pid}.json"));
        // A file left by an earlier process with this pid is replaced.
        std::fs::create_dir(&runtime).unwrap();
        std::fs::write(&json, "stale").unwrap();

        let files = ServerInfoFiles::write(&runtime, &info(dir.path(), Some("a&b")));
        assert_eq!(files.paths().len(), 2);
        let v: Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        assert_eq!(v["token"], "a&b");
        let html =
            std::fs::read_to_string(runtime.join(format!("jpserver-{pid}-open.html"))).unwrap();
        assert!(
            html.contains("url=http://127.0.0.1:8890/jupyter/lab?token=a%26b\""),
            "{html}"
        );
        #[cfg(unix)]
        for p in files.paths() {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", p.display());
        }
        drop(files);
        assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
    }

    #[test]
    fn the_runtime_dir_is_created_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("a/runtime");
        let files = ServerInfoFiles::write(&runtime, &info(dir.path(), None));
        assert_eq!(files.paths().len(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&runtime).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
