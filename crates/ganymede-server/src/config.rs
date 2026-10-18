//! Server configuration: a TOML file, overridden by command line flags.
//! Unknown keys are an error, so a misspelt `token` cannot start a server
//! with a random token.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ip: String,
    pub port: u16,
    /// Empty string disables auth. Unset generates a random token.
    pub token: Option<String>,
    /// Key for signing login cookies, created on first start so browsers
    /// stay logged in across restarts. Defaults to `jupyter_cookie_secret`
    /// in the Jupyter runtime dir, the file `jupyter_server` uses.
    pub cookie_secret_file: Option<PathBuf>,
    /// Contents root. Defaults to the current directory.
    pub root_dir: PathBuf,
    /// Checkpoints outside the root, for synced folders.
    pub checkpoint_dir: Option<PathBuf>,
    /// Where the notebook-signing key (`notebook_secret`) and the trusted
    /// signatures (`nbsignatures.db`) live. Defaults to the Jupyter data
    /// dir, so notebooks trusted by Jupyter are trusted here too
    /// (`NotebookNotary.data_dir`).
    pub notary_dir: Option<PathBuf>,
    /// Directory of static frontend assets served at `/`. When it is
    /// JupyterLab's static bundle (`share/jupyter/lab/static`), the server
    /// serves JupyterLab at `/lab` with its `/lab/api/*` endpoints instead.
    ///
    /// Unset, the newest JupyterLab installed by `ganymede lab install` is
    /// served, or `lab_version` if set; with none installed, only the API.
    pub static_dir: Option<PathBuf>,
    /// Which installed JupyterLab to serve when `static_dir` is unset.
    pub lab_version: Option<String>,
    /// Extra directories of prebuilt JupyterLab extensions, searched before
    /// those from `ganymede lab extension install` and `labextensions` in
    /// the Jupyter data dirs.
    pub labextensions_paths: Vec<PathBuf>,
    /// Where frontend state is stored: `nbconfig/`, `lab/user-settings/`,
    /// `lab/workspaces/`. Defaults to `JUPYTER_CONFIG_DIR` or `~/.jupyter`.
    pub config_dir: Option<PathBuf>,
    /// Prefix for a reverse proxy, e.g. `/jupyter`.
    pub base_url: String,
    /// Extra kernelspec roots searched before the Jupyter data dirs.
    pub kernelspec_roots: Vec<PathBuf>,
    /// Reported as `default` in `/api/kernelspecs`.
    pub default_kernel: Option<String>,
    /// Shut down kernels idle this long with no clients. 0 disables.
    pub cull_idle_secs: u64,
    /// Seconds a new kernel has to answer.
    pub startup_timeout_secs: u64,
    /// Discard kernel processes' own stdout and stderr (not cell output,
    /// which reaches clients over iopub). Otherwise both go to stderr.
    pub quiet: bool,
    pub allow_origins: Vec<String>,
    /// Accept any `Host` header. Unset, it is on unless `ip` is a loopback
    /// address, as in `jupyter_server`.
    pub allow_remote_access: Option<bool>,
    /// Host names accepted besides loopback addresses while
    /// `allow_remote_access` is off.
    pub local_hostnames: Vec<String>,
    /// Trust `X-Forwarded-Proto` and `X-Scheme` from a reverse proxy, as
    /// `jupyter_server`'s `trust_xheaders`: login cookies sent over a
    /// proxied HTTPS request are marked `Secure`.
    pub trust_xheaders: bool,
    /// Largest request body accepted, in bytes. A notebook is saved in one
    /// request, so this caps its size. `jupyter_server`'s default: 512 MiB.
    pub max_body_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            ip: "127.0.0.1".into(),
            port: 8888,
            token: None,
            cookie_secret_file: None,
            root_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            checkpoint_dir: None,
            notary_dir: None,
            static_dir: None,
            lab_version: None,
            labextensions_paths: vec![],
            config_dir: None,
            base_url: "/".into(),
            kernelspec_roots: vec![],
            default_kernel: None,
            cull_idle_secs: 0,
            startup_timeout_secs: 60,
            quiet: false,
            allow_origins: vec![],
            allow_remote_access: None,
            local_hostnames: vec!["localhost".into()],
            trust_xheaders: false,
            max_body_size: 512 * 1024 * 1024,
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// `allow_remote_access`, or whether `ip` listens beyond loopback:
    /// `jupyter_server`'s default.
    pub fn allow_remote_access(&self) -> bool {
        if let Some(v) = self.allow_remote_access {
            return v;
        }
        if self.ip.is_empty() {
            return true;
        }
        if let Ok(addr) = self.ip.parse::<std::net::IpAddr>() {
            return !addr.is_loopback();
        }
        use std::net::ToSocketAddrs;
        match (self.ip.as_str(), self.port).to_socket_addrs() {
            Ok(addrs) => addrs.into_iter().any(|a| !a.ip().is_loopback()),
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn toml_sets_keys_and_defaults_the_rest() {
        let cfg = Config::from_toml(
            "port = 0\ntoken = \"x\"\ncull_idle_secs = 30\nallow_origins = [\"https://a\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.port, 0);
        assert_eq!(cfg.token.as_deref(), Some("x"));
        assert_eq!(cfg.cull_idle_secs, 30);
        assert_eq!(cfg.allow_origins, ["https://a"]);
        assert_eq!(cfg.ip, "127.0.0.1");
        assert_eq!(cfg.startup_timeout_secs, 60);
        assert!(!cfg.quiet);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::from_toml("port = 0\ntokn = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("tokn"), "{err}");
        assert!(Config::from_toml("cull_idle_sec = 30").is_err());
    }

    #[test]
    fn wrong_types_are_rejected() {
        assert!(Config::from_toml("port = \"8888\"").is_err());
    }

    #[test]
    fn remote_access_defaults_to_whether_ip_is_loopback() {
        let with_ip = |ip: &str| Config {
            ip: ip.into(),
            ..Config::default()
        };
        assert!(!with_ip("127.0.0.1").allow_remote_access());
        assert!(!with_ip("::1").allow_remote_access());
        assert!(!with_ip("localhost").allow_remote_access());
        assert!(with_ip("0.0.0.0").allow_remote_access());
        assert!(with_ip("").allow_remote_access());
        assert!(with_ip("192.168.1.10").allow_remote_access());
        let mut cfg = with_ip("0.0.0.0");
        cfg.allow_remote_access = Some(false);
        assert!(!cfg.allow_remote_access());
    }
}
