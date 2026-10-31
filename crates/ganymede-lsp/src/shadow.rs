//! Virtual documents on disk. `jupyterlab-lsp` sends a notebook to a
//! language server as one document under `virtualDocumentsUri`, a URI that
//! names no real file. Servers that read files themselves (to resolve
//! imports, for example) need it on disk, so the text of every `didOpen`,
//! `didChange`, and `didSave` under that URI is written there first, as
//! `jupyter-lsp`'s shadow file system does.

use percent_encoding::percent_decode_str;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The messages that carry a document's text.
const WRITES: [&str; 3] = [
    "textDocument/didOpen",
    "textDocument/didChange",
    "textDocument/didSave",
];

pub struct Shadow {
    /// The directory, `<root>/.virtual_documents`.
    dir: PathBuf,
    /// Its `file://` URI, as the page config sends it.
    uri: String,
    /// Cleared on first use, so documents from an earlier run don't linger.
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

    /// Writes the document a client message carries, when it is one of
    /// `didOpen`, `didChange`, or `didSave` for a URI under the virtual documents URI. Other messages
    /// are left alone. Returns the path written.
    pub async fn apply(&self, message: &Value) -> std::io::Result<Option<PathBuf>> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(None);
        };
        if !WRITES.contains(&method) {
            return Ok(None);
        }
        let params = message.get("params");
        let doc = params.and_then(|p| p.get("textDocument"));
        let Some(uri) = doc.and_then(|d| d.get("uri")).and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(path) = self.path_for(uri)? else {
            return Ok(None);
        };
        // The text in full (didOpen, and didSave when the server asks for
        // it), else the first change (didChange); a later change in the
        // same message is ignored, as `jupyter-lsp` does.
        let change = match doc.and_then(|d| d.get("text")).and_then(Value::as_str) {
            Some(text) => Change { text, range: None },
            None if method == "textDocument/didChange" => {
                let Some(first) = params
                    .and_then(|p| p.get("contentChanges"))
                    .and_then(Value::as_array)
                    .and_then(|c| c.first())
                else {
                    return Ok(None);
                };
                let Some(text) = first.get("text").and_then(Value::as_str) else {
                    return Ok(None);
                };
                Change {
                    text,
                    range: first.get("range").and_then(range),
                }
            }
            None => return Ok(None),
        };
        self.clear_once().await?;
        let old = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let new = apply_change(&old, &change);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, new).await?;
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

    async fn clear_once(&self) -> std::io::Result<()> {
        let mut cleared = self.cleared.lock().await;
        if *cleared {
            return Ok(());
        }
        match tokio::fs::remove_dir_all(&self.dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        tokio::fs::create_dir_all(&self.dir).await?;
        *cleared = true;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

struct Change<'a> {
    text: &'a str,
    range: Option<((usize, usize), (usize, usize))>,
}

/// `{"start": {"line", "character"}, "end": {...}}`.
fn range(v: &Value) -> Option<((usize, usize), (usize, usize))> {
    let pos = |p: &Value| -> Option<(usize, usize)> {
        Some((
            usize::try_from(p.get("line")?.as_u64()?).ok()?,
            usize::try_from(p.get("character")?.as_u64()?).ok()?,
        ))
    };
    Some((pos(v.get("start")?)?, pos(v.get("end")?)?))
}

/// Replaces the range (all of `old` without one) with the change's text.
fn apply_change(old: &str, change: &Change) -> String {
    let Some((start, end)) = change.range else {
        return change.text.to_string();
    };
    let a = offset(old, start);
    let b = offset(old, end).max(a);
    let mut out = String::with_capacity(old.len() + change.text.len());
    out.push_str(&old[..a]);
    out.push_str(change.text);
    out.push_str(&old[b..]);
    out
}

/// The byte offset of an LSP position. `character` counts UTF-16 code
/// units, as the protocol's default encoding and the browser's strings
/// do. Past the end of a line is its end; past the last line is the end.
fn offset(text: &str, (line, character): (usize, usize)) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (i, c) in text[start..line_end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += c.len_utf16();
    }
    line_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shadow(dir: &Path) -> Shadow {
        let vd = dir.join(".virtual_documents");
        Shadow::new(vd.clone(), format!("file://{}", vd.display()))
    }

    #[test]
    fn range_edits_count_utf16() {
        let doc = "import os\nx = 'é😀'\n";
        let edit = |s, e, text| {
            apply_change(
                doc,
                &Change {
                    text,
                    range: Some((s, e)),
                },
            )
        };
        assert_eq!(edit((0, 7), (0, 9), "sys"), "import sys\nx = 'é😀'\n");
        // 😀 is two UTF-16 units: character 8 is after it.
        assert_eq!(edit((1, 5), (1, 8), "ab"), "import os\nx = 'ab'\n");
        assert_eq!(edit((9, 0), (9, 0), "end"), format!("{doc}end"));
        assert_eq!(edit((0, 99), (1, 0), ""), "import osx = 'é😀'\n");
    }

    #[tokio::test]
    async fn writes_virtual_documents_only() {
        let root = tempfile::tempdir().unwrap();
        let s = shadow(root.path());
        std::fs::create_dir_all(s.dir()).unwrap();
        std::fs::write(s.dir().join("stale.py"), "old").unwrap();
        let uri = format!("{}/sub/nb%20one.ipynb", s.uri);
        let open = json!({"method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": uri, "languageId": "python", "version": 0, "text": "a = 1\nb\n"}}});
        let path = s.apply(&open).await.unwrap().unwrap();
        assert_eq!(path, s.dir().join("sub").join("nb one.ipynb"));
        assert!(!s.dir().join("stale.py").exists(), "cleared on first use");
        let change = json!({"method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": 1},
            "contentChanges": [{"range": {"start": {"line": 1, "character": 0},
                "end": {"line": 1, "character": 1}}, "text": "bb"}]}});
        s.apply(&change).await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\nbb\n");
        let full = json!({"method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": "c"}]}});
        s.apply(&full).await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c");

        // A real file, another method, and an escape attempt.
        let real = json!({"method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": format!("file://{}/x.py", root.path().display()), "text": "x"}}});
        assert_eq!(s.apply(&real).await.unwrap(), None);
        let hover =
            json!({"method": "textDocument/hover", "params": {"textDocument": {"uri": uri}}});
        assert_eq!(s.apply(&hover).await.unwrap(), None);
        let escape = json!({"method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": format!("{}/%2E%2E/escaped.py", s.uri), "text": "x"}}});
        assert!(s.apply(&escape).await.is_err());
        assert!(!root.path().join("escaped.py").exists());
    }
}
