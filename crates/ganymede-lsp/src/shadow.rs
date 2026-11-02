//! Virtual documents on disk. `jupyterlab-lsp` sends a notebook to a
//! language server as one document under `virtualDocumentsUri`, a URI that
//! names no real file. Servers that read files themselves (to resolve
//! imports, for example) need it on disk, so each time the server's text
//! of a document under that URI changes (see `documents.rs`), it is
//! written there before the server gets the message, as `jupyter-lsp`'s
//! shadow file system does.

use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};

pub struct Shadow {
    /// The directory, `<root>/.virtual_documents`.
    dir: PathBuf,
    /// Its `file://` URI, as the page config sends it.
    uri: String,
    /// Whether the directory was cleared yet: on first use, so documents
    /// from an earlier run don't linger.
    cleared: tokio::sync::Mutex<bool>,
}

impl Shadow {
    pub fn new(dir: PathBuf, uri: String) -> Self {
        Shadow {
            dir,
            uri,
            cleared: tokio::sync::Mutex::new(false),
        }
    }

    /// Writes `text` as the document at `uri`, when that is under the
    /// virtual documents URI. Returns the path written.
    pub async fn write(&self, uri: &str, text: &str) -> std::io::Result<Option<PathBuf>> {
        let Some(path) = self.path_for(uri)? else {
            return Ok(None);
        };
        let mut cleared = self.cleared.lock().await;
        if !*cleared {
            self.clear().await?;
            *cleared = true;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, text).await?;
        Ok(Some(path))
    }

    /// The file for a virtual document URI, `None` for a URI outside the
    /// virtual documents, and an error for one that would leave them.
    fn path_for(&self, uri: &str) -> std::io::Result<Option<PathBuf>> {
        let decoded = percent_decode_str(uri).decode_utf8_lossy();
        let base = percent_decode_str(&self.uri).decode_utf8_lossy();
        let Some(rest) = decoded
            .strip_prefix(base.trim_end_matches('/'))
            .and_then(|r| r.strip_prefix('/'))
        else {
            return Ok(None);
        };
        let mut path = self.dir.clone();
        for part in rest.split('/') {
            let bad =
                part == ".." || part.contains('\\') || part.contains(':') || part.contains('\0');
            if bad {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{uri} is outside the virtual documents"),
                ));
            }
            if !part.is_empty() && part != "." {
                path.push(part);
            }
        }
        if path == self.dir {
            return Ok(None);
        }
        Ok(Some(path))
    }

    async fn clear(&self) -> std::io::Result<()> {
        match tokio::fs::remove_dir_all(&self.dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        tokio::fs::create_dir_all(&self.dir).await
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(dir: &Path) -> Shadow {
        let vd = dir.join(".virtual_documents");
        Shadow::new(vd.clone(), format!("file://{}", vd.display()))
    }

    #[tokio::test]
    async fn writes_virtual_documents_only() {
        let root = tempfile::tempdir().unwrap();
        let s = shadow(root.path());
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(s.dir().join("stale.py"), "old").unwrap();
        let uri = format!("{}/sub/nb%20one.ipynb", s.uri);
        let path = s.write(&uri, "a = 1\n").await.unwrap().unwrap();
        assert_eq!(path, s.dir().join("sub").join("nb one.ipynb"));
        assert!(!s.dir().join("stale.py").exists(), "cleared on first use");
        s.write(&uri, "c").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c");

        // A real file and an escape attempt.
        let real = format!("file://{}/x.py", root.path().display());
        assert_eq!(s.write(&real, "x").await.unwrap(), None);
        let escape = format!("{}/%2E%2E/escaped.py", s.uri);
        assert!(s.write(&escape, "x").await.is_err());
        assert!(!root.path().join("escaped.py").exists());
    }
}
