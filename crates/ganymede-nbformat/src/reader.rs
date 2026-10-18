//! Reading notebooks: lenient JSON parsing into [`Notebook`].

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

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
/// made it fail.
fn parse_json(bytes: &[u8]) -> Result<Value> {
    match serde_json::from_slice(bytes) {
        Ok(value) => Ok(value),
        Err(err) => match replace_lone_surrogates(bytes) {
            Some(fixed) => serde_json::from_slice(&fixed).map_err(|_| err.into()),
            None => Err(err.into()),
        },
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
        Self::from_value(parse_json(bytes)?)
    }

    /// Parse a notebook from a reader of JSON bytes (UTF-8).
    pub fn from_reader<R: Read>(mut reader: R) -> Result<Self> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Self::from_slice(&buf)
    }

    /// Convert an already parsed JSON value into a notebook.
    pub fn from_value(mut value: Value) -> Result<Self> {
        check_version(&value)?;
        strip_transient(&mut value);
        normalize_cell_ids(&mut value);
        Ok(serde_json::from_value(value)?)
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
