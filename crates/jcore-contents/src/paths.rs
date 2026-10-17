//! API path parsing and validation.
//!
//! An API path is what follows `/api/contents/`: forward-slash separated,
//! relative to the root, `""` for the root. [`ApiPath::parse`] is the only
//! way to turn client input into one, and it rejects everything that could
//! name a location outside the root before any filesystem call is made.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Normalize a path taken from a URL into API-path form: strips leading and
/// trailing slashes, collapses repeated slashes and drops `.` components.
///
/// `..` is left in place so that [`crate::Contents`] can reject it. An HTTP
/// layer should call this on the URL's path before calling the trait, since
/// `jupyter_server`'s route always captures a leading slash and strips it.
pub fn normalize_api_path(path: &str) -> String {
    path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// A validated API path: a list of plain components, none of which is empty,
/// `.`, `..`, or contains a slash or backslash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ApiPath {
    components: Vec<String>,
}

impl ApiPath {
    /// Parse and validate client input.
    ///
    /// Rejected with [`Error::Forbidden`]: backslashes anywhere, a leading
    /// slash (absolute path), and any `..` component. Rejected with
    /// [`Error::BadRequest`]: NUL bytes. Empty and `.` components are dropped.
    pub fn parse(path: &str) -> Result<Self> {
        if path.contains('\0') {
            return Err(Error::BadRequest("path contains a NUL byte".into()));
        }
        if path.contains('\\') {
            return Err(Error::Forbidden(format!(
                "backslashes are not allowed in paths: {path:?}"
            )));
        }
        if path.starts_with('/') {
            return Err(Error::Forbidden(format!(
                "absolute paths are not allowed: {path:?}"
            )));
        }
        let mut components = Vec::new();
        for component in path.split('/') {
            match component {
                "" | "." => continue,
                ".." => {
                    return Err(Error::Forbidden(format!(
                        "path escapes the contents root: {path:?}"
                    )))
                }
                c => components.push(c.to_owned()),
            }
        }
        Ok(Self { components })
    }

    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// Last component; `""` for the root.
    pub fn name(&self) -> &str {
        self.components.last().map(String::as_str).unwrap_or("")
    }

    /// The containing directory; the root's parent is the root.
    pub fn parent(&self) -> ApiPath {
        let mut components = self.components.clone();
        components.pop();
        ApiPath { components }
    }

    /// `self/name`. `name` must be a plain file name, which internal callers
    /// guarantee (directory entries and generated names).
    pub fn join(&self, name: &str) -> ApiPath {
        let mut components = self.components.clone();
        components.push(name.to_owned());
        ApiPath { components }
    }

    /// True when any component starts with `.`.
    pub fn is_hidden(&self) -> bool {
        self.components.iter().any(|c| c.starts_with('.'))
    }

    /// The path on disk under `root`. Safe because components cannot contain
    /// separators or be `..`.
    pub fn os_path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for c in &self.components {
            p.push(c);
        }
        p
    }
}

impl fmt::Display for ApiPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.components.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_api_path("/a/b/"), "a/b");
        assert_eq!(normalize_api_path("a//b/./c"), "a/b/c");
        assert_eq!(normalize_api_path("/"), "");
        assert_eq!(normalize_api_path(""), "");
        assert_eq!(normalize_api_path("../x"), "../x");
    }

    #[test]
    fn parse_accepts_plain_paths() {
        let p = ApiPath::parse("a/b.txt").unwrap();
        assert_eq!(p.to_string(), "a/b.txt");
        assert_eq!(p.name(), "b.txt");
        assert_eq!(p.parent().to_string(), "a");
        assert!(!p.is_hidden());
        assert_eq!(p.os_path(Path::new("/root")), Path::new("/root/a/b.txt"));

        let root = ApiPath::parse("").unwrap();
        assert!(root.is_root());
        assert_eq!(root.name(), "");
        assert_eq!(root.parent(), root);
        assert_eq!(ApiPath::parse("a/./b//").unwrap().to_string(), "a/b");
    }

    #[test]
    fn parse_rejects_escapes() {
        for bad in ["../x", "a/../../x", "a/..", "/etc/passwd", "a\\b", "..\\x"] {
            assert!(
                matches!(ApiPath::parse(bad), Err(Error::Forbidden(_))),
                "{bad:?} should be forbidden"
            );
        }
        assert!(matches!(ApiPath::parse("a\0b"), Err(Error::BadRequest(_))));
    }

    #[test]
    fn hidden_detection() {
        assert!(ApiPath::parse(".git/config").unwrap().is_hidden());
        assert!(ApiPath::parse("a/.hidden").unwrap().is_hidden());
        assert!(!ApiPath::parse("a/b.c").unwrap().is_hidden());
    }
}
