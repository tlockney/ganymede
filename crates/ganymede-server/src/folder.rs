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

/// Why a settings file couldn't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderError {
    /// The file's API path.
    pub file: String,
    pub why: String,
}

/// The default kernel for API directory `dir` under `root`: the `kernel`
/// of the nearest `.ganymede.toml` from `dir` up to the root. An error
/// names the file that can't be read or parsed, so a typo isn't silently
/// ignored. A `dir` the contents API would refuse (`..`, a backslash, a
/// NUL, and on Windows a `:`) has no default, and a folder that resolves
/// outside the root (through a symlink) is passed over.
pub fn default_kernel(root: &Path, dir: &str) -> Result<Option<FolderKernel>, FolderError> {
    if dir.contains(['\\', '\0']) || (cfg!(windows) && dir.contains(':')) {
        return Ok(None);
    }
    let parts: Vec<&str> = dir
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.contains(&"..") {
        return Ok(None);
    }
    let Ok(base) = root.canonicalize() else {
        return Ok(None);
    };
    for n in (0..=parts.len()).rev() {
        let folder = parts[..n].join("/");
        let api = if folder.is_empty() {
            FILE.to_owned()
        } else {
            format!("{folder}/{FILE}")
        };
        let fail = |why: String| FolderError {
            file: api.clone(),
            why,
        };
        let disk = root.join(&api);
        // Only a regular file: a symlink could point anywhere, and the
        // parse error would quote it.
        let meta = match std::fs::symlink_metadata(&disk) {
            Ok(m) => m,
            Err(_) => continue,
        };
        // The folders on the way may be links too: the file's folder must
        // really be under the root, as the contents API checks.
        match root.join(&folder).canonicalize() {
            Ok(real) if real.starts_with(&base) => {}
            _ => continue,
        }
        if !meta.file_type().is_file() {
            return Err(fail("not a regular file, so it is ignored".into()));
        }
        if meta.len() > MAX_BYTES {
            return Err(fail(format!("larger than {MAX_BYTES} bytes")));
        }
        let text =
            std::fs::read_to_string(&disk).map_err(|e| fail(format!("cannot read it: {e}")))?;
        let settings: Settings = toml::from_str(&text).map_err(|e| fail(e.message().to_owned()))?;
        if let Some(kernel) = settings.kernel.map(|k| k.trim().to_owned()) {
            if kernel.is_empty() {
                return Err(fail("`kernel` is empty".into()));
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

    fn kernel(root: &Path, dir: &str) -> Option<String> {
        default_kernel(root, dir).unwrap().map(|k| k.kernel)
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
        assert_eq!(kernel(root, "").as_deref(), Some("cdp-node"));
    }

    #[test]
    fn mistakes_are_errors_naming_the_file() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        write(root, "a/.ganymede.toml", "kernal = \"python3\"\n");
        let e = default_kernel(root, "a").unwrap_err();
        assert_eq!(e.file, "a/.ganymede.toml");
        assert!(e.why.contains("kernal"), "{e:?}");
        write(root, "a/.ganymede.toml", "kernel = \"  \"\n");
        assert!(default_kernel(root, "a").unwrap_err().why.contains("empty"));
        write(
            root,
            "a/.ganymede.toml",
            &format!("# {}\n", "x".repeat(20_000)),
        );
        assert!(default_kernel(root, "a")
            .unwrap_err()
            .why
            .contains("larger"));
    }

    #[test]
    fn never_reads_above_the_root_or_through_links() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), ".ganymede.toml", "kernel = \"outside\"\n");
        let root = t.path().join("root");
        std::fs::create_dir_all(root.join("a")).unwrap();
        assert_eq!(kernel(&root, "a"), None);
        assert_eq!(kernel(&root, "../"), None);
        assert_eq!(kernel(&root, "a/../.."), None);
        // A backslash is refused whatever it means on this system: on Unix,
        // a real folder named `a\b` holding a file.
        write(&root, "a\\b/.ganymede.toml", "kernel = \"backslash\"\n");
        assert_eq!(kernel(&root, "a\\b"), None);
        assert_eq!(kernel(&root, "..\\.."), None);
        assert_eq!(kernel(&root, "a\0"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(
                t.path().join(".ganymede.toml"),
                root.join("a/.ganymede.toml"),
            )
            .unwrap();
            assert!(default_kernel(&root, "a")
                .unwrap_err()
                .why
                .contains("not a regular file"));
            // A folder under the root that links outside it.
            let outside = t.path().join("outside");
            write(&outside, ".ganymede.toml", "kernel = \"elsewhere\"\n");
            symlink(&outside, root.join("linked")).unwrap();
            assert_eq!(kernel(&root, "linked"), None);
            assert_eq!(kernel(&root, "linked/deeper"), None);
            // A link that stays inside the root is fine.
            write(&root, "real/.ganymede.toml", "kernel = \"inside\"\n");
            symlink(root.join("real"), root.join("alias")).unwrap();
            assert_eq!(kernel(&root, "alias").as_deref(), Some("inside"));
        }
    }

    #[test]
    fn dir_of_paths() {
        assert_eq!(dir_of("x.ipynb"), "");
        assert_eq!(dir_of("journal/x.ipynb"), "journal");
        assert_eq!(dir_of("a/b/x.ipynb"), "a/b");
    }
}
