//! Per-folder settings: a `.ganymede.toml` in a folder under the server
//! root. For now it holds one key, the kernel new notebooks in that
//! folder start:
//!
//! ```toml
//! kernel = "python3"
//! ```
//!
//! The nearest file wins, from the notebook's folder up to the root; a
//! file without `kernel` passes the question to the folder above. Files
//! above the root are never read. The file is hidden, so the contents
//! API doesn't serve it; it is read from disk, and only when the
//! contents are a local directory.

use serde::Deserialize;
use std::path::Path;

/// The settings file's name.
pub const FILE: &str = ".ganymede.toml";

/// Larger files are an error, not read.
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    kernel: Option<String>,
}

/// A folder's default kernel and the settings file that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderKernel {
    pub kernel: String,
    /// The file's API path, such as `journal/.ganymede.toml`.
    pub file: String,
}

/// The default kernel for API directory `dir` under `root`: the `kernel`
/// of the nearest `.ganymede.toml` from `dir` up to the root. An error
/// names the file that can't be read or parsed, so a typo isn't silently
/// ignored.
pub fn default_kernel(root: &Path, dir: &str) -> Result<Option<FolderKernel>, String> {
    let parts: Vec<&str> = dir
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.contains(&"..") {
        return Ok(None);
    }
    for n in (0..=parts.len()).rev() {
        let folder = parts[..n].join("/");
        let api = if folder.is_empty() {
            FILE.to_owned()
        } else {
            format!("{folder}/{FILE}")
        };
        let disk = root.join(&api);
        // Only a regular file: a symlink could point anywhere, and the
        // parse error would quote it.
        let meta = match std::fs::symlink_metadata(&disk) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_file() {
            return Err(format!("{api} is not a regular file, so it is ignored"));
        }
        if meta.len() > MAX_BYTES {
            return Err(format!("{api} is larger than {MAX_BYTES} bytes"));
        }
        let text = std::fs::read_to_string(&disk).map_err(|e| format!("cannot read {api}: {e}"))?;
        let settings: Settings =
            toml::from_str(&text).map_err(|e| format!("{api}: {}", e.message()))?;
        if let Some(kernel) = settings.kernel.map(|k| k.trim().to_owned()) {
            if kernel.is_empty() {
                return Err(format!("{api}: `kernel` is empty"));
            }
            return Ok(Some(FolderKernel { kernel, file: api }));
        }
    }
    Ok(None)
}

/// The API directory a notebook path is in: `""` at the root.
pub fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, api: &str, text: &str) {
        let p = root.join(api);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn nearest_file_with_a_kernel_wins() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        assert_eq!(default_kernel(root, "a/b").unwrap(), None);
        write(root, ".ganymede.toml", "kernel = \"cdp-node\"\n");
        write(root, "a/.ganymede.toml", "kernel = \"python3\"\n");
        // No `kernel`: the folder above decides.
        write(root, "a/b/.ganymede.toml", "# nothing yet\n");
        let k = default_kernel(root, "a/b").unwrap().unwrap();
        assert_eq!(k.kernel, "python3");
        assert_eq!(k.file, "a/.ganymede.toml");
        let k = default_kernel(root, "c").unwrap().unwrap();
        assert_eq!(
            (k.kernel.as_str(), k.file.as_str()),
            ("cdp-node", ".ganymede.toml")
        );
        assert_eq!(
            default_kernel(root, "").unwrap().unwrap().kernel,
            "cdp-node"
        );
    }

    #[test]
    fn mistakes_are_errors_naming_the_file() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        write(root, "a/.ganymede.toml", "kernal = \"python3\"\n");
        let e = default_kernel(root, "a").unwrap_err();
        assert!(e.starts_with("a/.ganymede.toml: "), "{e}");
        write(root, "a/.ganymede.toml", "kernel = \"  \"\n");
        assert!(default_kernel(root, "a").unwrap_err().contains("empty"));
        write(
            root,
            "a/.ganymede.toml",
            &format!("# {}\n", "x".repeat(20_000)),
        );
        assert!(default_kernel(root, "a").unwrap_err().contains("larger"));
    }

    #[test]
    fn never_reads_above_the_root_or_through_links() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), ".ganymede.toml", "kernel = \"outside\"\n");
        let root = t.path().join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        assert_eq!(default_kernel(&root, "a").unwrap(), None);
        assert_eq!(default_kernel(&root, "../").unwrap(), None);
        assert_eq!(default_kernel(&root, "a/../..").unwrap(), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                t.path().join(".ganymede.toml"),
                root.join("a/.ganymede.toml"),
            )
            .unwrap();
            assert!(default_kernel(&root, "a")
                .unwrap_err()
                .contains("not a regular file"));
        }
    }

    #[test]
    fn dir_of_paths() {
        assert_eq!(dir_of("x.ipynb"), "");
        assert_eq!(dir_of("journal/x.ipynb"), "journal");
        assert_eq!(dir_of("a/b/x.ipynb"), "a/b");
    }
}
