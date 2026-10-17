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
    /// Contents root. Defaults to the current directory.
    pub root_dir: PathBuf,
    /// Checkpoints outside the root, for synced folders.
    pub checkpoint_dir: Option<PathBuf>,
    /// Directory of static frontend assets served at `/`.
    pub static_dir: Option<PathBuf>,
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
            root_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            checkpoint_dir: None,
            static_dir: None,
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
