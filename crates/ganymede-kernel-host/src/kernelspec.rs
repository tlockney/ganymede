//! Kernelspec discovery from the Jupyter data directories.
//!
//! Search order matches `jupyter_client`, minus the Python `sys.prefix`
//! root: `JUPYTER_PATH` entries, the user data dir (`JUPYTER_DATA_DIR` or the
//! platform default), then the system dirs. Earlier roots win on a name
//! clash. Extra roots can be added for environments the host does not know
//! how to scan (conda, venv) without the host learning what those are.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterruptMode {
    #[default]
    Signal,
    Message,
}

/// The parsed `kernel.json` plus where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kernelspec {
    /// Directory name, which is the name clients use.
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub resource_dir: PathBuf,
    pub argv: Vec<String>,
    pub display_name: String,
    pub language: String,
    #[serde(default)]
    pub interrupt_mode: InterruptMode,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Free-form. cdp-kernel puts `runtime`, `debugger`, and its version
    /// here; a capability manifest belongs here too. Passed through.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Kernelspec {
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join("kernel.json");
        let text = std::fs::read_to_string(&path)?;
        let mut spec: Kernelspec =
            serde_json::from_str(&text).map_err(|e| Error::InvalidKernelspec {
                path: path.clone(),
                reason: e.to_string(),
            })?;
        if spec.argv.is_empty() {
            return Err(Error::InvalidKernelspec {
                path,
                reason: "argv is empty".into(),
            });
        }
        spec.name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        spec.resource_dir = dir.to_path_buf();
        Ok(spec)
    }

    /// argv with `{connection_file}` and `{resource_dir}` substituted.
    pub fn command(&self, connection_file: &Path) -> Vec<String> {
        self.argv
            .iter()
            .map(|a| {
                a.replace("{connection_file}", &connection_file.to_string_lossy())
                    .replace("{resource_dir}", &self.resource_dir.to_string_lossy())
            })
            .collect()
    }
}

pub trait KernelspecSource: Send + Sync {
    fn list(&self) -> Result<Vec<Kernelspec>>;

    /// Case-insensitive lookup by name, as `jupyter_client` does.
    fn get(&self, name: &str) -> Result<Kernelspec> {
        self.list()?
            .into_iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::NoSuchKernelspec(name.to_string()))
    }
}

/// Kernelspecs under `<root>/kernels/<name>/kernel.json` for each root.
#[derive(Debug, Clone)]
pub struct FsKernelspecs {
    roots: Vec<PathBuf>,
}

impl FsKernelspecs {
    /// The standard Jupyter data directories plus `extra_roots` searched
    /// first.
    pub fn discover(extra_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut roots: Vec<PathBuf> = extra_roots.into_iter().collect();
        roots.extend(jupyter_data_dirs());
        FsKernelspecs { roots }
    }

    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        FsKernelspecs { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

impl KernelspecSource for FsKernelspecs {
    fn list(&self) -> Result<Vec<Kernelspec>> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for root in &self.roots {
            let dir = root.join("kernels");
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            for path in entries {
                match Kernelspec::from_dir(&path) {
                    Ok(spec) => {
                        if seen.insert(spec.name.to_ascii_lowercase()) {
                            out.push(spec);
                        }
                    }
                    Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!("skipping kernelspec at {}: {e}", path.display()),
                }
            }
        }
        Ok(out)
    }
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The user data directory: `JUPYTER_DATA_DIR`, else the platform default.
pub fn jupyter_user_data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("JUPYTER_DATA_DIR") {
        return Some(PathBuf::from(d));
    }
    if cfg!(target_os = "macos") {
        return home().map(|h| h.join("Library").join("Jupyter"));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("jupyter"));
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(x).join("jupyter"));
    }
    home().map(|h| h.join(".local").join("share").join("jupyter"))
}

/// All data directories in search order.
pub fn jupyter_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(p) = std::env::var_os("JUPYTER_PATH") {
        dirs.extend(std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()));
    }
    dirs.extend(jupyter_user_data_dir());
    if cfg!(windows) {
        if let Some(pd) = std::env::var_os("PROGRAMDATA") {
            dirs.push(PathBuf::from(pd).join("jupyter"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/local/share/jupyter"));
        dirs.push(PathBuf::from("/usr/share/jupyter"));
    }
    dirs
}

/// The config directory: `JUPYTER_CONFIG_DIR`, else `~/.jupyter`. Holds
/// frontend state the server stores for clients (`nbconfig`, JupyterLab
/// settings and workspaces), not server configuration.
pub fn jupyter_config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("JUPYTER_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    home()
        .map(|h| h.join(".jupyter"))
        .unwrap_or_else(|| PathBuf::from(".jupyter"))
}

/// Where connection files go: `JUPYTER_RUNTIME_DIR`, else `<user data>/runtime`.
pub fn jupyter_runtime_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("JUPYTER_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    jupyter_user_data_dir()
        .map(|d| d.join("runtime"))
        .unwrap_or_else(std::env::temp_dir)
}

/// Copy a kernelspec directory (one holding `kernel.json`) into
/// `<data_dir>/kernels/<name>`, as `jupyter kernelspec install` does.
/// `name` defaults to the source directory's name and is lowercased.
/// Returns the installed directory.
pub fn install(
    source: &Path,
    name: Option<&str>,
    data_dir: &Path,
    replace: bool,
) -> Result<PathBuf> {
    // Validates the source before anything is written.
    Kernelspec::from_dir(source)?;
    let name = match name {
        Some(n) => n.to_string(),
        None => source
            .canonicalize()?
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
    .to_ascii_lowercase();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(Error::InvalidKernelspec {
            path: source.to_path_buf(),
            reason: format!("invalid kernelspec name {name:?}: use letters, digits, '.', '_', '-'"),
        });
    }
    let dest = data_dir.join("kernels").join(&name);
    if dest.exists() {
        if !replace {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists; pass replace to overwrite", dest.display()),
            )));
        }
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir(source, &dest)?;
    Ok(dest)
}

/// Delete the named kernelspec from the first root that has it. Returns the
/// directory removed.
pub fn remove(source: &dyn KernelspecSource, name: &str) -> Result<PathBuf> {
    let spec = source.get(name)?;
    std::fs::remove_dir_all(&spec.resource_dir)?;
    Ok(spec.resource_dir)
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_spec(root: &Path, name: &str, json: &str) {
        let d = root.join("kernels").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("kernel.json"), json).unwrap();
    }

    #[test]
    fn lists_specs_with_precedence_and_passes_metadata_through() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_spec(
            a.path(),
            "cdp-node",
            r#"{"argv":["node","k.js","{connection_file}"],"display_name":"TS (node)","language":"typescript","interrupt_mode":"message","metadata":{"runtime":"node","capabilities":{"jit":true}}}"#,
        );
        write_spec(
            b.path(),
            "CDP-NODE",
            r#"{"argv":["other"],"display_name":"shadowed","language":"typescript"}"#,
        );
        write_spec(
            b.path(),
            "python3",
            r#"{"argv":["python","-m","ipykernel_launcher","-f","{connection_file}"],"display_name":"Python 3","language":"python","env":{"X":"1"}}"#,
        );
        write_spec(b.path(), "broken", r#"{"display_name":"no argv"}"#);
        let src = FsKernelspecs::with_roots(vec![a.path().into(), b.path().into()]);
        let specs = src.list().unwrap();
        let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["cdp-node", "python3"]);
        let ts = src.get("CDP-node").unwrap();
        assert_eq!(ts.interrupt_mode, InterruptMode::Message);
        assert_eq!(ts.metadata["capabilities"]["jit"], Value::Bool(true));
        assert_eq!(
            ts.command(Path::new("/tmp/k.json")),
            vec!["node", "k.js", "/tmp/k.json"]
        );
        let py = src.get("python3").unwrap();
        assert_eq!(py.interrupt_mode, InterruptMode::Signal);
        assert_eq!(py.env["X"], "1");
        assert!(matches!(src.get("nope"), Err(Error::NoSuchKernelspec(_))));
    }

    #[test]
    fn install_and_remove() {
        let src = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let spec_dir = src.path().join("My-Kernel");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(
            spec_dir.join("kernel.json"),
            r#"{"argv":["k","{connection_file}"],"display_name":"K","language":"k"}"#,
        )
        .unwrap();
        std::fs::write(spec_dir.join("logo-32x32.png"), b"png").unwrap();
        let dest = install(&spec_dir, None, data.path(), false).unwrap();
        assert!(dest.ends_with("kernels/my-kernel"));
        assert!(dest.join("logo-32x32.png").exists());
        assert!(install(&spec_dir, None, data.path(), false).is_err());
        install(&spec_dir, None, data.path(), true).unwrap();
        assert!(install(&spec_dir, Some("bad name"), data.path(), false).is_err());

        let specs = FsKernelspecs::with_roots(vec![data.path().into()]);
        assert_eq!(specs.get("my-kernel").unwrap().display_name, "K");
        assert_eq!(remove(&specs, "MY-KERNEL").unwrap(), dest);
        assert!(specs.list().unwrap().is_empty());
    }
}
