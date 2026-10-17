//! Server configuration: a TOML file, overridden by command line flags.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
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
    /// Directory of static frontend assets served at `/`. When it is
    /// JupyterLab's static bundle (`share/jupyter/lab/static`), the server
    /// serves JupyterLab at `/lab` with its `/lab/api/*` endpoints instead.
    pub static_dir: Option<PathBuf>,
    /// Extra directories of prebuilt JupyterLab extensions, searched before
    /// `labextensions` in the Jupyter data dirs.
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
    pub startup_timeout_secs: u64,
    pub allow_origins: Vec<String>,
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
            static_dir: None,
            labextensions_paths: vec![],
            config_dir: None,
            base_url: "/".into(),
            kernelspec_roots: vec![],
            default_kernel: None,
            cull_idle_secs: 0,
            startup_timeout_secs: 60,
            allow_origins: vec![],
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}
