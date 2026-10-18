//! Reading notebooks: lenient JSON parsing into [`Notebook`].

use std::collections::HashSet;
use std::fmt;
use std::io::Read;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::types::{Notebook, NBFORMAT};
use crate::{Error, Result};

/// Remove transient values that nbformat never stores (`strip_transient`).
///
/// Applied on both read and write: `orig_nbformat`, `orig_nbformat_minor` and
/// `signature` in the notebook metadata, and `trusted` in each cell's
/// metadata.
pub(crate) fn strip_transient(nb: &mut Value) {
    if let Some(Value::Object(metadata)) = nb.get_mut("metadata") {
        metadata.remove("orig_nbformat");
        metadata.remove("orig_nbformat_minor");
        metadata.remove("signature");
    }
    if let Some(Value::Array(cells)) = nb.get_mut("cells") {
        for cell in cells {
            if let Some(Value::Object(metadata)) = cell.get_mut("metadata") {
                metadata.remove("trusted");
            }
        }
    }
}

/// A new cell id: 8 hex digits, as nbformat's `generate_corpus_id`
/// (`uuid.uuid4().hex[:8]`).
fn generate_cell_id() -> String {
    let mut id = uuid::Uuid::new_v4().simple().to_string();
    id.truncate(8);
    id
}

/// nbformat's `validator._normalize` cell id repair, on the JSON tree.
///
/// For nbformat 4.5 and later, a cell without an `id` (or with a `null` one)
/// gets a fresh one, and a cell whose id repeats an earlier cell's gets a
/// fresh one too. Earlier minor versions have no cell ids and are left
/// alone. Applied on both read and write, as nbformat does. New ids also
/// avoid every id already in the notebook, so the repair cannot create a
/// duplicate of a later cell.
pub(crate) fn normalize_cell_ids(nb: &mut Value) {
    let major = nb.get("nbformat").and_then(Value::as_u64);
    let minor = nb.get("nbformat_minor").and_then(Value::as_u64);
    if major != Some(u64::from(NBFORMAT)) || minor.is_none_or(|m| m < 5) {
        return;
    }
    let Some(Value::Array(cells)) = nb.get_mut("cells") else {
        return;
    };
    let mut taken: HashSet<String> = cells
        .iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    for cell in cells {
        let Some(cell) = cell.as_object_mut() else {
            continue;
        };
        let keep = match cell.get("id") {
            None | Some(Value::Null) => false,
            Some(Value::String(id)) => seen.insert(id.clone()),
            // A non-string id is invalid but, like nbformat, left as is.
            Some(_) => true,
        };
        if keep {
            continue;
        }
        let id = loop {
            let id = generate_cell_id();
            if taken.insert(id.clone()) {
                break id;
            }
        };
        seen.insert(id.clone());
        cell.insert("id".to_owned(), Value::String(id));
    }
}

/// Replace lone UTF-16 surrogate escapes (`"\ud800"`) with `"\ufffd"`.
///
/// Python's `json` accepts them; serde_json, whose strings must be valid
/// UTF-8, does not. Returns `None` when there are none, so callers only pay
/// for a second parse when the first one failed for this reason. Backslashes
/// only occur inside JSON strings, so the scan needs no string state.
fn replace_lone_surrogates(bytes: &[u8]) -> Option<Vec<u8>> {
    fn hex4(b: &[u8]) -> Option<u16> {
        let digits = b.get(..4)?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u16::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let unit = (bytes.get(i + 1) == Some(&b'u'))
            .then(|| hex4(&bytes[i + 2..]))
            .flatten();
        let Some(unit) = unit else {
            // Some other escape (or a malformed one): copy it whole so an
            // escaped backslash is never mistaken for an escape's start.
            let end = (i + 2).min(bytes.len());
            out.extend_from_slice(&bytes[i..end]);
            i = end;
            continue;
        };
        let paired = (0xD800..0xDC00).contains(&unit)
            && bytes[i + 6..].starts_with(b"\\u")
            && hex4(&bytes[i + 8..]).is_some_and(|low| (0xDC00..0xE000).contains(&low));
        if paired {
            out.extend_from_slice(&bytes[i..i + 12]);
            i += 12;
        } else if (0xD800..0xE000).contains(&unit) {
            out.extend_from_slice(b"\\ufffd");
            changed = true;
            i += 6;
        } else {
            out.extend_from_slice(&bytes[i..i + 6]);
            i += 6;
        }
    }
    changed.then_some(out)
}

/// Parse JSON text, retrying with lone surrogates replaced if that is what
/// made it fail. The flag says whether any were replaced.
fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<(T, bool)> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Ok((value, false)),
        Err(err) => match replace_lone_surrogates(bytes) {
            Some(fixed) => serde_json::from_slice(&fixed)
                .map(|v| (v, true))
                .map_err(|_| err),
            None => Err(err),
        },
    }
}

/// Parse JSON as Python's `json` would, as far as UTF-8 allows: a lone
/// UTF-16 surrogate escape (`"\ud800"`), which serde_json rejects, reads as
/// U+FFFD. For request bodies that carry a notebook, so a notebook that
/// reads from disk can also be saved back. On any other error, the error
/// of the first parse is returned.
pub fn from_json_slice<T: DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    parse_json(bytes).map(|(value, _)| value)
}

/// One way a notebook broke the nbformat v4 schema, and what the reader did
/// about it (default, empty value, or kept as is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadIssue {
    /// Where, in Python's `jsonschema` notation: `['cells'][1]['outputs'][0]`;
    /// empty for the notebook itself or the JSON text.
    pub path: String,
    /// What was wrong, worded like `jsonschema`'s messages.
    pub message: String,
}

impl fmt::Display for ReadIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{} (on instance{})", self.message, self.path)
        }
    }
}

/// What the lenient reader repaired or defaulted while reading a notebook.
///
/// Empty for a notebook that fits the schema. Missing and duplicate cell
/// ids are not reported: nbformat repairs those before validating too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadReport {
    pub issues: Vec<ReadIssue>,
}

impl ReadReport {
    /// At most this many issues are spelled out in [`Self::validation_message`].
    const MAX_LISTED: usize = 20;

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    /// jupyter_server's `message` for a notebook that failed validation
    /// (`"Notebook validation failed: ..."`), or `None` when nothing was
    /// wrong.
    pub fn validation_message(&self) -> Option<String> {
        if self.issues.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = self
            .issues
            .iter()
            .take(Self::MAX_LISTED)
            .map(ToString::to_string)
            .collect();
        if self.issues.len() > Self::MAX_LISTED {
            lines.push(format!(
                "... and {} more",
                self.issues.len() - Self::MAX_LISTED
            ));
        }
        Some(format!("Notebook validation failed: {}", lines.join("\n")))
    }

    fn push(&mut self, path: &str, message: String) {
        self.issues.push(ReadIssue {
            path: path.to_owned(),
            message,
        });
    }
}

/// The JSON type a schema field wants.
#[derive(Clone, Copy)]
enum Want {
    Object,
    Array,
    String,
    /// A string or an array of strings.
    Multiline,
    /// An array of strings.
    Lines,
    /// A non-negative integer or `null`.
    ExecutionCount,
}

/// `jsonschema`-style rendering of a value inside a message.
fn repr(value: &Value) -> String {
    let text = match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(s) => format!("'{s}'"),
        other => other.to_string(),
    };
    if text.chars().count() > 60 {
        format!("{}...", text.chars().take(60).collect::<String>())
    } else {
        text
    }
}

/// Report `obj[key]` if it is missing or not what `want` says.
fn check_field(
    obj: &Map<String, Value>,
    key: &str,
    want: Want,
    path: &str,
    report: &mut ReadReport,
) {
    let Some(value) = obj.get(key) else {
        report.push(path, format!("'{key}' is a required property"));
        return;
    };
    let is_lines = |v: &Value| v.as_array().is_some_and(|a| a.iter().all(Value::is_string));
    let (ok, type_name) = match want {
        Want::Object => (value.is_object(), "'object'"),
        Want::Array => (value.is_array(), "'array'"),
        Want::String => (value.is_string(), "'string'"),
        Want::Multiline => (value.is_string() || is_lines(value), "'string' or 'array'"),
        Want::Lines => (is_lines(value), "'array' of 'string'"),
        Want::ExecutionCount => {
            if value.as_i64().is_some_and(|n| n < 0) {
                report.push(
                    &format!("{path}['{key}']"),
                    format!("{} is less than the minimum of 0", repr(value)),
                );
                return;
            }
            (value.is_null() || value.is_u64(), "'integer' or 'null'")
        }
    };
    if !ok {
        report.push(
            &format!("{path}['{key}']"),
            format!("{} is not of type {type_name}", repr(value)),
        );
    }
}

/// Check an object-valued entry, reporting it when it is not an object.
fn as_object<'a>(
    value: &'a Value,
    path: &str,
    report: &mut ReadReport,
) -> Option<&'a Map<String, Value>> {
    let obj = value.as_object();
    if obj.is_none() {
        report.push(path, format!("{} is not of type 'object'", repr(value)));
    }
    obj
}

fn check_output(output: &Value, path: &str, report: &mut ReadReport) {
    let Some(obj) = as_object(output, path, report) else {
        return;
    };
    let fields: &[(&str, Want)] = match obj.get("output_type").and_then(Value::as_str) {
        Some("stream") => &[("name", Want::String), ("text", Want::Multiline)],
        Some("display_data") => &[("data", Want::Object), ("metadata", Want::Object)],
        Some("execute_result") => &[
            ("execution_count", Want::ExecutionCount),
            ("data", Want::Object),
            ("metadata", Want::Object),
        ],
        Some("error") => &[
            ("ename", Want::String),
            ("evalue", Want::String),
            ("traceback", Want::Lines),
        ],
        Some(other) => {
            report.push(path, format!("'{other}' is not a known output_type"));
            return;
        }
        None => {
            check_field(obj, "output_type", Want::String, path, report);
            return;
        }
    };
    for &(key, want) in fields {
        check_field(obj, key, want, path, report);
    }
}

fn check_cell(cell: &Value, path: &str, report: &mut ReadReport) {
    let Some(obj) = as_object(cell, path, report) else {
        return;
    };
    let code = match obj.get("cell_type").and_then(Value::as_str) {
        Some("code") => true,
        Some("markdown" | "raw") => false,
        Some(other) => {
            report.push(path, format!("'{other}' is not a known cell_type"));
            return;
        }
        None => {
            check_field(obj, "cell_type", Want::String, path, report);
            return;
        }
    };
    check_field(obj, "metadata", Want::Object, path, report);
    check_field(obj, "source", Want::Multiline, path, report);
    if !code {
        return;
    }
    check_field(obj, "execution_count", Want::ExecutionCount, path, report);
    check_field(obj, "outputs", Want::Array, path, report);
    if let Some(Value::Array(outputs)) = obj.get("outputs") {
        for (i, output) in outputs.iter().enumerate() {
            check_output(output, &format!("{path}['outputs'][{i}]"), report);
        }
    }
}

/// The schema checks behind [`ReadReport`]: the places where the lenient
/// reader defaults, empties or keeps verbatim what the nbformat v4 schema
/// rejects. Run after the cell id repair, as nbformat validates after it.
fn check_notebook(nb: &Map<String, Value>, report: &mut ReadReport) {
    check_field(nb, "metadata", Want::Object, "", report);
    check_field(nb, "cells", Want::Array, "", report);
    if let Some(Value::Array(cells)) = nb.get("cells") {
        for (i, cell) in cells.iter().enumerate() {
            check_cell(cell, &format!("['cells'][{i}]"), report);
        }
    }
}

fn check_version(value: &Value) -> Result<()> {
    let Some(obj) = value.as_object() else {
        return Err(Error::NotAnObject);
    };
    let Some(major) = obj.get("nbformat") else {
        return Err(Error::MissingVersion);
    };
    let Some(major) = major.as_u64() else {
        return Err(Error::InvalidVersion(major.clone()));
    };
    if major != u64::from(NBFORMAT) {
        // TODO: upgrade nbformat 3 (and older) notebooks instead of rejecting
        // them, as nbformat.convert does. Out of scope for now.
        let minor = obj
            .get("nbformat_minor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Err(Error::UnsupportedVersion { major, minor });
    }
    Ok(())
}

impl Notebook {
    /// Parse a notebook from its JSON text.
    ///
    /// Sources and text-like values may be strings or arrays of strings.
    /// Transient metadata is stripped and, for nbformat 4.5 and later,
    /// missing or duplicate cell ids are repaired, as nbformat does. Values
    /// that break the schema are read leniently (see the [`crate::Output`]
    /// docs); a lone surrogate escape, which Python's `json` accepts, is read
    /// as U+FFFD. Only nbformat 4 is accepted; see
    /// [`Error::UnsupportedVersion`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        Self::from_slice(s.as_bytes())
    }

    /// Parse a notebook from JSON bytes (UTF-8).
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        Ok(Self::from_slice_with_report(bytes)?.0)
    }

    /// [`Notebook::from_slice`], also returning what the lenient reader
    /// repaired: jupyter_server's validation `message`.
    pub fn from_slice_with_report(bytes: &[u8]) -> Result<(Self, ReadReport)> {
        let (value, replaced) = parse_json(bytes)?;
        let (nb, mut report) = Self::from_value_with_report(value)?;
        if replaced {
            report.issues.insert(
                0,
                ReadIssue {
                    path: String::new(),
                    message: "lone surrogate escapes were read as U+FFFD".to_owned(),
                },
            );
        }
        Ok((nb, report))
    }

    /// Parse a notebook from a reader of JSON bytes (UTF-8).
    pub fn from_reader<R: Read>(mut reader: R) -> Result<Self> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Self::from_slice(&buf)
    }

    /// Convert an already parsed JSON value into a notebook.
    pub fn from_value(value: Value) -> Result<Self> {
        Ok(Self::from_value_with_report(value)?.0)
    }

    /// [`Notebook::from_value`], also returning what the lenient reader
    /// repaired.
    pub fn from_value_with_report(mut value: Value) -> Result<(Self, ReadReport)> {
        check_version(&value)?;
        strip_transient(&mut value);
        normalize_cell_ids(&mut value);
        let mut report = ReadReport::default();
        if let Some(nb) = value.as_object() {
            check_notebook(nb, &mut report);
        }
        Ok((serde_json::from_value(value)?, report))
    }

    /// Read a notebook file.
    pub fn read_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_slice(&bytes)
    }
}

impl std::str::FromStr for Notebook {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Notebook::from_str(s)
    }
}

/// Read a notebook file. Same as [`Notebook::read_file`].
pub fn read_file(path: impl AsRef<Path>) -> Result<Notebook> {
    Notebook::read_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Cell;
    use serde_json::json;

    #[test]
    fn rejects_other_major_versions() {
        let v3 = r#"{"nbformat": 3, "nbformat_minor": 0, "metadata": {}, "worksheets": []}"#;
        match Notebook::from_str(v3) {
            Err(Error::UnsupportedVersion { major: 3, minor: 0 }) => {}
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            Notebook::from_str(r#"{"nbformat": 5, "nbformat_minor": 1, "cells": []}"#),
            Err(Error::UnsupportedVersion { major: 5, minor: 1 })
        ));
        assert!(matches!(
            Notebook::from_str(r#"{"cells": []}"#),
            Err(Error::MissingVersion)
        ));
        assert!(matches!(
            Notebook::from_str(r#"{"nbformat": "4"}"#),
            Err(Error::InvalidVersion(_))
        ));
        assert!(matches!(Notebook::from_str("[]"), Err(Error::NotAnObject)));
        assert!(matches!(
            Notebook::from_str("{not json"),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn strips_transient_metadata() {
        let nb = Notebook::from_value(json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {"orig_nbformat": 4, "orig_nbformat_minor": 2, "signature": "x", "keep": 1},
            "cells": [
                {"cell_type": "markdown", "metadata": {"trusted": true, "tags": []}, "source": "hi"},
            ],
        }))
        .unwrap();
        assert_eq!(
            nb.metadata,
            json!({"keep": 1}).as_object().cloned().unwrap()
        );
        let Cell::Markdown(md) = &nb.cells[0] else {
            panic!()
        };
        assert_eq!(
            md.metadata,
            json!({"tags": []}).as_object().cloned().unwrap()
        );
        // nbformat 4.5: the missing id was filled in.
        let id = md.id.as_deref().unwrap();
        assert!(id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn replaces_only_lone_surrogates() {
        // `%` stands for a backslash, to keep the JSON escapes readable.
        let fix = |s: &str| {
            replace_lone_surrogates(s.replace('%', "\\").as_bytes())
                .map(|v| String::from_utf8(v).unwrap().replace('\\', "%"))
        };
        assert_eq!(fix(r#""a%u00e9%n%"""#), None);
        assert_eq!(fix(r#""%ud83d%ude80""#), None);
        assert_eq!(fix(r#""%%ud800""#), None);
        assert_eq!(fix(r#""%ud800""#).as_deref(), Some(r#""%ufffd""#));
        assert_eq!(fix(r#""%udc00x""#).as_deref(), Some(r#""%ufffdx""#));
        assert_eq!(
            fix(r#""%ud800%ud800%udc00""#).as_deref(),
            Some(r#""%ufffd%ud800%udc00""#)
        );
        assert_eq!(fix(r#""%ud800%n""#).as_deref(), Some(r#""%ufffd%n""#));
        assert_eq!(fix(r#""%%%ud800""#).as_deref(), Some(r#""%%%ufffd""#));
        // Truncated escapes are copied and left for the parser to reject.
        assert_eq!(fix(r#""%ud8"#), None);
        assert_eq!(fix("%"), None);
    }

    #[test]
    fn reads_lone_surrogates_as_replacement_characters() {
        let text =
            r#"{"nbformat": 4, "nbformat_minor": 4, "metadata": {"s": "\udfff!"}, "cells": []}"#;
        let nb = Notebook::from_str(text).unwrap();
        assert_eq!(nb.metadata["s"], "\u{fffd}!");
        // Any other syntax error is still reported as the original error.
        assert!(matches!(
            Notebook::from_str(r#"{"nbformat": 4, "x": "\ud800", }"#),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn parses_via_from_str_trait() {
        let nb: Notebook = r#"{"nbformat": 4, "nbformat_minor": 4, "metadata": {}, "cells": []}"#
            .parse()
            .unwrap();
        assert_eq!(nb.nbformat_minor, 4);
        assert!(nb.cells.is_empty());
    }
}
