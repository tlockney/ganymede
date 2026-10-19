//! nbformat v4 data model.
//!
//! Every struct carries an `extra` map captured with `#[serde(flatten)]`, so
//! fields this crate does not know about survive a read/write round trip.
//!
//! Like Python nbformat, which only logs validation errors, deserializing is
//! lenient: `null` where an object, array or string belongs reads as empty,
//! missing output fields take defaults, an `execution_count` that is not a
//! non-negative integer reads as integral or `None`, and an output that still
//! does not fit the schema is kept verbatim as [`Output::Unknown`] (a cell as
//! [`Cell::Unknown`]).

use std::collections::BTreeMap;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::multiline::MultilineString;

/// The nbformat major version this crate reads and writes.
pub const NBFORMAT: u32 = 4;
/// The nbformat minor version new notebooks are created with.
pub const NBFORMAT_MINOR: u32 = 5;

/// Deserialize `null` as the type's default (empty map, list or string).
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Read an `execution_count` leniently.
///
/// The schema allows a non-negative integer or `null`. An integral float
/// such as `1.0` reads as that integer; anything else (`-1`, `1.5`, a
/// string) reads as `None`, so it is written back as `null`.
fn lenient_execution_count<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let Value::Number(n) = value else {
        return Ok(None);
    };
    if let Some(n) = n.as_u64() {
        return Ok(Some(n));
    }
    Ok(n.as_f64()
        .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f < u64::MAX as f64)
        .map(|f| f as u64))
}

/// A notebook document (nbformat 4.x).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub nbformat: u32,
    pub nbformat_minor: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    pub metadata: Map<String, Value>,
    #[serde(default, deserialize_with = "null_as_default")]
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
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Code(CodeCell),
    Markdown(MarkdownCell),
    Raw(RawCell),
    /// A cell this crate cannot type: an unknown `cell_type` (such as
    /// nbformat 3's `heading` in a file that says it is nbformat 4), or a known one whose fields do not fit the
    /// schema even leniently. Kept as read and written back unchanged, as
    /// Python nbformat does.
    Unknown(Value),
}

impl Cell {
    pub fn id(&self) -> Option<&str> {
        match self {
            Cell::Code(c) => c.id.as_deref(),
            Cell::Markdown(c) => c.id.as_deref(),
            Cell::Raw(c) => c.id.as_deref(),
            Cell::Unknown(v) => v.get("id").and_then(Value::as_str),
        }
    }

    /// The cell's source; `None` for an [`Cell::Unknown`] cell.
    pub fn source(&self) -> Option<&MultilineString> {
        match self {
            Cell::Code(c) => Some(&c.source),
            Cell::Markdown(c) => Some(&c.source),
            Cell::Raw(c) => Some(&c.source),
            Cell::Unknown(_) => None,
        }
    }

    /// The cell's metadata; for an [`Cell::Unknown`] cell, its `metadata`
    /// if that is an object.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        match self {
            Cell::Code(c) => Some(&c.metadata),
            Cell::Markdown(c) => Some(&c.metadata),
            Cell::Raw(c) => Some(&c.metadata),
            Cell::Unknown(v) => v.get("metadata").and_then(Value::as_object),
        }
    }

    /// Mutable access to the cell's metadata, as [`Cell::metadata`].
    pub fn metadata_mut(&mut self) -> Option<&mut Map<String, Value>> {
        match self {
            Cell::Code(c) => Some(&mut c.metadata),
            Cell::Markdown(c) => Some(&mut c.metadata),
            Cell::Raw(c) => Some(&mut c.metadata),
            Cell::Unknown(v) => v.get_mut("metadata").and_then(Value::as_object_mut),
        }
    }

    /// The `cell_type` tag as written on disk, if there is one.
    pub fn cell_type(&self) -> Option<&str> {
        match self {
            Cell::Code(_) => Some("code"),
            Cell::Markdown(_) => Some("markdown"),
            Cell::Raw(_) => Some("raw"),
            Cell::Unknown(v) => v.get("cell_type").and_then(Value::as_str),
        }
    }
}

/// The typed cells, as serde derives their tagged form.
#[derive(Serialize)]
#[serde(tag = "cell_type", rename_all = "lowercase")]
enum TypedCellRef<'a> {
    Code(&'a CodeCell),
    Markdown(&'a MarkdownCell),
    Raw(&'a RawCell),
}

#[derive(Deserialize)]
#[serde(tag = "cell_type", rename_all = "lowercase")]
enum TypedCell {
    Code(CodeCell),
    Markdown(MarkdownCell),
    Raw(RawCell),
}

impl Serialize for Cell {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Cell::Code(c) => TypedCellRef::Code(c).serialize(serializer),
            Cell::Markdown(c) => TypedCellRef::Markdown(c).serialize(serializer),
            Cell::Raw(c) => TypedCellRef::Raw(c).serialize(serializer),
            Cell::Unknown(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Cell {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match TypedCell::deserialize(&value) {
            Ok(TypedCell::Code(c)) => Cell::Code(c),
            Ok(TypedCell::Markdown(c)) => Cell::Markdown(c),
            Ok(TypedCell::Raw(c)) => Cell::Raw(c),
            Err(_) => Cell::Unknown(value),
        })
    }
}

/// A `code` cell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CodeCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub source: MultilineString,
    /// Always written, as `null` when the cell has not run.
    #[serde(default, deserialize_with = "lenient_execution_count")]
    pub execution_count: Option<u64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub outputs: Vec<Output>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `markdown` cell.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MarkdownCell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
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
    #[serde(default, deserialize_with = "null_as_default")]
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
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Stream(StreamOutput),
    DisplayData(DisplayDataOutput),
    ExecuteResult(ExecuteResultOutput),
    Error(ErrorOutput),
    /// An output this crate cannot type: an unknown `output_type`, or a
    /// known one whose fields do not fit the schema even leniently. Kept as
    /// read and written back unchanged, as Python nbformat does.
    Unknown(Value),
}

impl Output {
    /// The `output_type` tag, if there is one.
    pub fn output_type(&self) -> Option<&str> {
        match self {
            Output::Stream(_) => Some("stream"),
            Output::DisplayData(_) => Some("display_data"),
            Output::ExecuteResult(_) => Some("execute_result"),
            Output::Error(_) => Some("error"),
            Output::Unknown(v) => v.get("output_type").and_then(Value::as_str),
        }
    }
}

/// The typed outputs, as serde derives their tagged form.
#[derive(Serialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
enum TypedOutputRef<'a> {
    Stream(&'a StreamOutput),
    DisplayData(&'a DisplayDataOutput),
    ExecuteResult(&'a ExecuteResultOutput),
    Error(&'a ErrorOutput),
}

#[derive(Deserialize)]
#[serde(tag = "output_type", rename_all = "snake_case")]
enum TypedOutput {
    Stream(StreamOutput),
    DisplayData(DisplayDataOutput),
    ExecuteResult(ExecuteResultOutput),
    Error(ErrorOutput),
}

impl Serialize for Output {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Output::Stream(o) => TypedOutputRef::Stream(o).serialize(serializer),
            Output::DisplayData(o) => TypedOutputRef::DisplayData(o).serialize(serializer),
            Output::ExecuteResult(o) => TypedOutputRef::ExecuteResult(o).serialize(serializer),
            Output::Error(o) => TypedOutputRef::Error(o).serialize(serializer),
            Output::Unknown(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Output {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match TypedOutput::deserialize(&value) {
            Ok(TypedOutput::Stream(o)) => Output::Stream(o),
            Ok(TypedOutput::DisplayData(o)) => Output::DisplayData(o),
            Ok(TypedOutput::ExecuteResult(o)) => Output::ExecuteResult(o),
            Ok(TypedOutput::Error(o)) => Output::Error(o),
            Err(_) => Output::Unknown(value),
        })
    }
}

/// A `stream` output (stdout or stderr text).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamOutput {
    /// `"stdout"` or `"stderr"`. A stream without one reads as `"stdout"`,
    /// nbformat's `new_output` default.
    #[serde(default = "default_stream_name")]
    pub name: String,
    #[serde(default)]
    pub text: MultilineString,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_stream_name() -> String {
    "stdout".to_owned()
}

/// A `display_data` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayDataOutput {
    #[serde(default, deserialize_with = "null_as_default")]
    pub data: MimeBundle,
    #[serde(default, deserialize_with = "null_as_default")]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// An `execute_result` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecuteResultOutput {
    #[serde(default, deserialize_with = "lenient_execution_count")]
    pub execution_count: Option<u64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub data: MimeBundle,
    #[serde(default, deserialize_with = "null_as_default")]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// An `error` output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorOutput {
    /// Empty when missing on disk.
    #[serde(default)]
    pub ename: String,
    /// Empty when missing on disk.
    #[serde(default)]
    pub evalue: String,
    /// Lines of the traceback, usually with ANSI escapes. Not split further.
    #[serde(default, deserialize_with = "null_as_default")]
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
    fn unknown_cells_are_kept_verbatim() {
        for raw in [
            json!({"cell_type": "heading", "level": 1, "source": ["a\n", "b"], "metadata": {"k": 1}}),
            json!({"cell_type": "code", "source": 5}),
            json!({"no_cell_type": true}),
            json!(42),
        ] {
            let cell: Cell = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(cell, Cell::Unknown(raw.clone()));
            assert_eq!(serde_json::to_value(&cell).unwrap(), raw);
        }
        let heading: Cell = serde_json::from_value(
            json!({"cell_type": "heading", "id": "h", "metadata": {"k": 1}}),
        )
        .unwrap();
        assert_eq!(heading.cell_type(), Some("heading"));
        assert_eq!(heading.id(), Some("h"));
        assert_eq!(heading.metadata().unwrap()["k"], 1);
        assert_eq!(heading.source(), None);
    }
}
