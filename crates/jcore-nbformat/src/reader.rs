//! Reading notebooks: lenient JSON parsing into [`Notebook`].

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
    /// Transient metadata is stripped as nbformat does. Only nbformat 4 is
    /// accepted; see [`Error::UnsupportedVersion`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(s)?;
        Self::from_value(value)
    }

    /// Parse a notebook from JSON bytes (UTF-8).
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        Self::from_value(value)
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
        assert_eq!(md.id, None);
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
