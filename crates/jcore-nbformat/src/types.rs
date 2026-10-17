//! nbformat v4 data model.
//!
//! Every struct carries an `extra` map captured with `#[serde(flatten)]`, so
//! fields this crate does not know about survive a read/write round trip.

use std::collections::BTreeMap;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::multiline::MultilineString;

/// The nbformat major version this crate reads and writes.
pub const NBFORMAT: u32 = 4;
/// The nbformat minor version new notebooks are created with.
pub const NBFORMAT_MINOR: u32 = 5;

/// A notebook document (nbformat 4.x).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub nbformat: u32,
    pub nbformat_minor: u32,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub cells: Vec<Cell>,
    /// Fields not covered by the schema, preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Notebook {
    /// An empty nbformat 4.5 notebook.
    pub fn new() -> Self {
        Notebook {
            nbformat: NBFORMAT,
            nbformat_minor: NBFORMAT_MINOR,
            metadata: Map::new(),
            cells: Vec::new(),
            extra: Map::new(),
        }
    }
}

impl Default for Notebook {
    fn default() -> Self {
        Self::new()
    }
}

/// A notebook cell, tagged on disk by `cell_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cell_type", rename_all = "lowercase")]
pub enum Cell {
    Code(CodeCell),
    Markdown(MarkdownCell),
    Raw(RawCell),
}

impl Cell {
    pub fn id(&self) -> Option<&str> {
        match self {
            Cell::Code(c) => c.id.as_deref(),
            Cell::Markdown(c) => c.id.as_deref(),
            Cell::Raw(c) => c.id.as_deref(),
        }
    }

    pub fn source(&self) -> &MultilineString {
        match self {
            Cell::Code(c) => &c.source,
            Cell::Markdown(c) => &c.source,
            Cell::Raw(c) => &c.source,
        }
    }

    pub fn metadata(&self) -> &Map<String, Value> {
        match self {
            Cell::Code(c) => &c.metadata,
            Cell::Markdown(c) => &c.metadata,
            Cell::Raw(c) => &c.metadata,
        }
    }

    pub fn metadata_mut(&mut self) -> &mut Map<String, Value> {
        match self {
            Cell::Code(c) => &mut c.metadata,
            Cell::Markdown(c) => &mut c.metadata,
            Cell::Raw(c) => &mut c.metadata,
        }
    }

    /// The `cell_type` tag as written on disk.
    pub fn cell_type(&self) -> &'static str {
        match self {
            Cell::Code(_) => "code",
            Cell::Markdown(_) => "markdown",
            Cell::Raw(_) => "raw",
        }
    }
}

/// A `code` cell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CodeCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub source: MultilineString,
    /// Always written, as `null` when the cell has not run.
    #[serde(default)]
    pub execution_count: Option<u64>,
    #[serde(default)]
    pub outputs: Vec<Output>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `markdown` cell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MarkdownCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub source: MultilineString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Attachments>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `raw` cell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RawCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub source: MultilineString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Attachments>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Cell attachments: file name to MIME bundle.
pub type Attachments = BTreeMap<String, MimeBundle>;

/// A cell output, tagged on disk by `output_type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
pub enum Output {
    Stream(StreamOutput),
    DisplayData(DisplayDataOutput),
    ExecuteResult(ExecuteResultOutput),
    Error(ErrorOutput),
}

/// A `stream` output (stdout or stderr text).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamOutput {
    /// `"stdout"` or `"stderr"`.
    pub name: String,
    #[serde(default)]
    pub text: MultilineString,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `display_data` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayDataOutput {
    #[serde(default)]
    pub data: MimeBundle,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// An `execute_result` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecuteResultOutput {
    #[serde(default)]
    pub execution_count: Option<u64>,
    #[serde(default)]
    pub data: MimeBundle,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// An `error` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorOutput {
    pub ename: String,
    pub evalue: String,
    /// Lines of the traceback, usually with ANSI escapes. Not split further.
    #[serde(default)]
    pub traceback: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Whether a MIME type stores JSON data (never joined or split as text).
///
/// Mirrors nbformat's `_is_json_mime`.
pub fn is_json_mime(mime: &str) -> bool {
    mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))
}

/// Whether a MIME type's string value is written as an array of lines.
///
/// Mirrors nbformat's `_split_mimebundle`: `text/*`, `application/javascript`
/// and `image/svg+xml`.
pub fn is_split_mime(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "application/javascript" || mime == "image/svg+xml"
}

/// A map from MIME type to data.
///
/// Text-like entries are plain strings in memory. On disk they may be arrays
/// of line fragments; deserializing joins any array of strings under a
/// non-JSON MIME type (nbformat's `_rejoin_mimebundle`). JSON MIME types
/// (`application/json`, `application/*+json`) hold arbitrary JSON values.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct MimeBundle(pub BTreeMap<String, Value>);

impl MimeBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, mime: &str) -> Option<&Value> {
        self.0.get(mime)
    }

    /// The entry as text, if it is a string.
    pub fn get_str(&self, mime: &str) -> Option<&str> {
        self.0.get(mime).and_then(Value::as_str)
    }

    pub fn insert(&mut self, mime: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        self.0.insert(mime.into(), value.into())
    }

    pub fn contains(&self, mime: &str) -> bool {
        self.0.contains_key(mime)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Join array-of-string values under non-JSON MIME types into strings.
    pub fn rejoin(&mut self) {
        for (mime, value) in self.0.iter_mut() {
            if is_json_mime(mime) {
                continue;
            }
            if let Value::Array(items) = value {
                if items.iter().all(Value::is_string) {
                    let joined: String = items.iter().filter_map(Value::as_str).collect();
                    *value = Value::String(joined);
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for MimeBundle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut bundle = MimeBundle(BTreeMap::deserialize(deserializer)?);
        bundle.rejoin();
        Ok(bundle)
    }
}

impl FromIterator<(String, Value)> for MimeBundle {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut bundle = MimeBundle(iter.into_iter().collect());
        bundle.rejoin();
        bundle
    }
}

impl IntoIterator for MimeBundle {
    type Item = (String, Value);
    type IntoIter = std::collections::btree_map::IntoIter<String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mime_classification() {
        assert!(is_json_mime("application/json"));
        assert!(is_json_mime("application/vnd.jupyter.widget-view+json"));
        assert!(!is_json_mime("text/json"));
        assert!(!is_json_mime("application/javascript"));
        assert!(is_split_mime("text/plain"));
        assert!(is_split_mime("text/html"));
        assert!(is_split_mime("application/javascript"));
        assert!(is_split_mime("image/svg+xml"));
        assert!(!is_split_mime("image/png"));
        assert!(!is_split_mime("application/json"));
    }

    #[test]
    fn mimebundle_rejoins_on_read() {
        let b: MimeBundle = serde_json::from_value(json!({
            "text/plain": ["a\n", "b"],
            "image/png": ["iVBOR", "w0KGgo="],
            "application/json": ["kept", "as", "array"],
            "application/vnd.x+json": ["kept"],
            "application/vnd.mixed": ["a", 1],
            "text/html": "<p>already a string</p>",
        }))
        .unwrap();
        assert_eq!(b.get_str("text/plain"), Some("a\nb"));
        assert_eq!(b.get_str("image/png"), Some("iVBORw0KGgo="));
        assert_eq!(
            b.get("application/json"),
            Some(&json!(["kept", "as", "array"]))
        );
        assert_eq!(b.get("application/vnd.x+json"), Some(&json!(["kept"])));
        assert_eq!(b.get("application/vnd.mixed"), Some(&json!(["a", 1])));
        assert_eq!(b.get_str("text/html"), Some("<p>already a string</p>"));
    }

    #[test]
    fn cells_and_outputs_keep_unknown_fields() {
        let cell: Cell = serde_json::from_value(json!({
            "cell_type": "code",
            "id": "x",
            "metadata": {},
            "source": ["a\n", "b"],
            "execution_count": null,
            "outputs": [
                {"output_type": "stream", "name": "stdout", "text": ["1\n"], "weird": 1},
                {"output_type": "error", "ename": "E", "evalue": "v", "traceback": ["t"]},
            ],
            "future_field": {"k": "v"},
        }))
        .unwrap();
        let Cell::Code(code) = &cell else {
            panic!("expected code cell")
        };
        assert_eq!(code.source, "a\nb");
        assert_eq!(code.extra.get("future_field"), Some(&json!({"k": "v"})));
        // The tag is not duplicated into `extra`.
        assert!(!code.extra.contains_key("cell_type"));
        let Output::Stream(s) = &code.outputs[0] else {
            panic!("expected stream")
        };
        assert_eq!(s.text, "1\n");
        assert_eq!(s.extra.get("weird"), Some(&json!(1)));
        assert!(!s.extra.contains_key("output_type"));

        let back = serde_json::to_value(&cell).unwrap();
        assert_eq!(back["cell_type"], "code");
        assert_eq!(back["future_field"], json!({"k": "v"}));
        assert_eq!(back["outputs"][0]["output_type"], "stream");
        assert_eq!(back["outputs"][0]["weird"], 1);
    }

    #[test]
    fn unknown_cell_type_is_an_error() {
        assert!(
            serde_json::from_value::<Cell>(json!({"cell_type": "heading", "source": ""})).is_err()
        );
    }
}
