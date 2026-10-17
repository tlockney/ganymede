//! The `/api/contents` wire model, field for field as `jupyter_server`
//! produces and consumes it.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `type` of a contents entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentType {
    Notebook,
    File,
    Directory,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Notebook => "notebook",
            ContentType::File => "file",
            ContentType::Directory => "directory",
        }
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `format` of a model's content: `json` for notebooks and directory
/// listings, `text` or `base64` for files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Text,
    Base64,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Text => "text",
            Format::Base64 => "base64",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of the contents API, serialized with `jupyter_server`'s field
/// names.
///
/// - `path` is the API path: forward slashes, no leading slash, `""` for the
///   root. `name` is its last component (`""` for the root).
/// - `created` and `last_modified` are RFC 3339 UTC timestamps with
///   microseconds, e.g. `2026-10-17T00:00:00.123456Z`.
/// - `mimetype` is `null` for notebooks and directories, guessed from the
///   extension for files (`text/plain` or `application/octet-stream` when
///   nothing matches and content was requested).
/// - `format` and `content` are `null` when content was not requested.
///   With content: notebooks carry the notebook JSON object, files a string,
///   directories an array of child models without content.
/// - `size` is in bytes and `null` for directories.
/// - `hash` / `hash_algorithm` are omitted entirely when absent (they are
///   optional in `jupyter_server` 2.11+ and this crate does not compute them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentsModel {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub kind: ContentType,
    pub created: String,
    pub last_modified: String,
    pub mimetype: Option<String>,
    pub format: Option<Format>,
    pub content: Option<Value>,
    pub writable: bool,
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algorithm: Option<String>,
}

impl ContentsModel {
    /// The same model with `content` and `format` cleared, as children of a
    /// directory listing and the result of a save are returned.
    pub fn without_content(mut self) -> Self {
        self.content = None;
        self.format = None;
        self
    }
}

/// Options for [`crate::Contents::get`], mirroring the query parameters of
/// `GET /api/contents/{path}`.
///
/// `Default` requests no content; note that the HTTP API's default for
/// `?content=` is `1`, so an HTTP layer should set `content: true` unless the
/// query says otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GetOptions {
    /// Include the content (`?content=1`).
    pub content: bool,
    /// Force the kind (`?type=`). `Some(Directory)` on a file or
    /// `Some(File | Notebook)` on a directory is a `BadRequest`. `Some(File)`
    /// on an `.ipynb` returns its raw text instead of parsed JSON.
    pub type_hint: Option<ContentType>,
    /// Requested format for files (`?format=`). `text` fails with
    /// `BadRequest` on non-UTF-8 bytes; `None` falls back to `base64` for
    /// them. Ignored for notebooks and directories.
    pub format: Option<Format>,
}

impl GetOptions {
    /// Content included, no type or format hint.
    pub fn with_content() -> Self {
        Self {
            content: true,
            ..Self::default()
        }
    }
}

/// Body of `PUT /api/contents/{path}`.
///
/// Unknown fields (`name`, `path`, `chunk`, ...) are ignored on
/// deserialization.
///
/// Conflict detection: when `last_modified` is present it is compared with
/// the file's current mtime, and a file that is newer by more than 500 ms
/// fails with [`crate::Error::Conflict`] (409), so a client editing a stale
/// copy does not silently overwrite changes another writer (iCloud, another
/// client, an editor) made in the meantime. When it is absent the client's
/// copy wins: the save is unconditional, which is what clients that never
/// send it, such as VS Code, get.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveRequest {
    #[serde(rename = "type")]
    pub kind: ContentType,
    /// `json` for notebooks, `text` or `base64` for files (required for
    /// files), ignored for directories.
    #[serde(default)]
    pub format: Option<Format>,
    /// Notebook JSON object, file string, or absent for directories.
    #[serde(default)]
    pub content: Option<Value>,
    /// RFC 3339 timestamp of the model the client is saving over; see above.
    #[serde(default)]
    pub last_modified: Option<String>,
}

impl SaveRequest {
    /// A notebook save from its JSON value.
    pub fn notebook(content: Value) -> Self {
        Self {
            kind: ContentType::Notebook,
            format: Some(Format::Json),
            content: Some(content),
            last_modified: None,
        }
    }

    /// A UTF-8 text file save.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            kind: ContentType::File,
            format: Some(Format::Text),
            content: Some(Value::String(content.into())),
            last_modified: None,
        }
    }

    /// A binary file save from already base64-encoded content.
    pub fn base64(content: impl Into<String>) -> Self {
        Self {
            kind: ContentType::File,
            format: Some(Format::Base64),
            content: Some(Value::String(content.into())),
            last_modified: None,
        }
    }

    /// A directory creation.
    pub fn directory() -> Self {
        Self {
            kind: ContentType::Directory,
            format: None,
            content: None,
            last_modified: None,
        }
    }

    /// Attach the client's `last_modified` for conflict detection.
    pub fn with_last_modified(mut self, last_modified: impl Into<String>) -> Self {
        self.last_modified = Some(last_modified.into());
        self
    }
}

/// One checkpoint, as `GET /api/contents/{path}/checkpoints` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub last_modified: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_serializes_with_jupyter_server_field_names() {
        let model = ContentsModel {
            name: "a.txt".into(),
            path: "dir/a.txt".into(),
            kind: ContentType::File,
            created: "2026-10-17T00:00:00.000001Z".into(),
            last_modified: "2026-10-17T00:00:00.000002Z".into(),
            mimetype: Some("text/plain".into()),
            format: Some(Format::Text),
            content: Some(Value::String("hi".into())),
            writable: true,
            size: Some(2),
            hash: None,
            hash_algorithm: None,
        };
        let json = serde_json::to_value(&model).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "name",
                "path",
                "type",
                "created",
                "last_modified",
                "mimetype",
                "format",
                "content",
                "writable",
                "size"
            ]
        );
        assert_eq!(json["type"], "file");
        assert_eq!(json["format"], "text");
        let back: ContentsModel = serde_json::from_value(json).unwrap();
        assert_eq!(back, model);
    }

    #[test]
    fn nulls_round_trip() {
        let json = serde_json::json!({
            "name": "", "path": "", "type": "directory",
            "created": "2026-10-17T00:00:00.000000Z",
            "last_modified": "2026-10-17T00:00:00.000000Z",
            "mimetype": null, "format": null, "content": null,
            "writable": true, "size": null
        });
        let model: ContentsModel = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(model.kind, ContentType::Directory);
        assert_eq!(model.content, None);
        assert_eq!(serde_json::to_value(&model).unwrap(), json);
    }

    #[test]
    fn save_request_ignores_unknown_fields() {
        let req: SaveRequest = serde_json::from_value(serde_json::json!({
            "type": "file", "format": "text", "content": "x",
            "name": "x.txt", "path": "x.txt", "chunk": 1
        }))
        .unwrap();
        assert_eq!(req, SaveRequest::text("x"));
    }
}
