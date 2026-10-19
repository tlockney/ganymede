//! Upgrading nbformat 3 notebooks to 4.5 on read.
//!
//! What `nbformat.reads(s, as_version=4)` does for a v3 file: the v3 reader's
//! `strip_transient` and `rejoin_lines`, then `v4.convert.upgrade`. The
//! result keeps `orig_nbformat` and `orig_nbformat_minor` in the notebook
//! metadata, as Python's does; the writer strips them, as nbformat's does.
//! Where Python would raise on a malformed v3 notebook, values are left as
//! they are for the lenient v4 reader to report.

use serde_json::{Map, Value};

use crate::multiline::splitlines;
use crate::types::{NBFORMAT, NBFORMAT_MINOR};

/// v3 output keys whose values may be split into lists of lines
/// (`v3.rwbase._multiline_outputs`).
const MULTILINE_OUTPUTS: [&str; 6] = ["text", "html", "svg", "latex", "javascript", "json"];

/// v3 output keys and the MIME types they become (`v4.convert._mime_map`).
const MIME_MAP: [(&str, &str); 8] = [
    ("text", "text/plain"),
    ("html", "text/html"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpeg", "image/jpeg"),
    ("latex", "text/latex"),
    ("json", "application/json"),
    ("javascript", "application/javascript"),
];

/// Upgrade a parsed nbformat 3 notebook in place to nbformat 4.5. Cells get
/// no ids here; the v4 id repair that runs next gives each a fresh one, as
/// `upgrade_cell` does.
pub(crate) fn upgrade_v3(nb: &mut Map<String, Value>) {
    // v3 `strip_transient`.
    nb.remove("orig_nbformat");
    nb.remove("orig_nbformat_minor");
    let worksheets = match nb.remove("worksheets") {
        Some(Value::Array(ws)) => ws,
        _ => Vec::new(),
    };
    // Flatten the worksheets, as `upgrade` does.
    let mut cells = Vec::new();
    for ws in worksheets {
        if let Value::Object(mut ws) = ws {
            if let Some(Value::Array(ws_cells)) = ws.remove("cells") {
                cells.extend(ws_cells);
            }
        }
    }
    for cell in &mut cells {
        if let Value::Object(cell) = cell {
            if let Some(Value::Object(metadata)) = cell.get_mut("metadata") {
                metadata.remove("trusted");
            }
            cell.remove("trusted");
            rejoin_cell(cell);
            upgrade_cell(cell);
        }
    }

    let metadata = nb
        .entry("metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(metadata) = metadata {
        metadata.remove("name");
        metadata.remove("signature");
        // The v3 transient keys were stripped above, so these are constant.
        metadata.insert("orig_nbformat".into(), 3.into());
        metadata.insert("orig_nbformat_minor".into(), 0.into());
    }
    nb.insert("nbformat".into(), NBFORMAT.into());
    nb.insert("nbformat_minor".into(), NBFORMAT_MINOR.into());
    nb.insert("cells".into(), Value::Array(cells));
}

/// v3 `rejoin_lines` for one cell.
fn rejoin_cell(cell: &mut Map<String, Value>) {
    if cell.get("cell_type").and_then(Value::as_str) == Some("code") {
        join_field(cell, "input");
        if let Some(Value::Array(outputs)) = cell.get_mut("outputs") {
            for output in outputs.iter_mut().filter_map(Value::as_object_mut) {
                for key in MULTILINE_OUTPUTS {
                    join_field(output, key);
                }
            }
        }
    } else {
        join_field(cell, "source");
        join_field(cell, "rendered");
    }
}

/// v3 `_join_lines`: lines from `splitlines(True)` are concatenated, lines
/// from a plain `splitlines()` are joined with newlines.
fn join_field(obj: &mut Map<String, Value>, key: &str) {
    let Some(Value::Array(items)) = obj.get(key) else {
        return;
    };
    let Some(lines) = items
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<&str>>>()
    else {
        return;
    };
    let sep = match lines.first() {
        Some(first) if first.ends_with(['\n', '\r']) => "",
        _ => "\n",
    };
    let joined = lines.join(sep);
    obj.insert(key.to_owned(), Value::String(joined));
}

/// `v4.convert.upgrade_cell`.
fn upgrade_cell(cell: &mut Map<String, Value>) {
    cell.entry("metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    cell.remove("id");
    match cell.get("cell_type").and_then(Value::as_str) {
        Some("code") => {
            cell.remove("language");
            if let Some(collapsed) = cell.remove("collapsed") {
                if let Some(Value::Object(metadata)) = cell.get_mut("metadata") {
                    metadata.insert("collapsed".into(), collapsed);
                }
            }
            let source = cell.remove("input").unwrap_or_else(|| "".into());
            cell.insert("source".into(), source);
            let count = cell.remove("prompt_number").unwrap_or(Value::Null);
            cell.insert("execution_count".into(), count);
            if let Some(Value::Array(outputs)) = cell.get_mut("outputs") {
                for output in outputs.iter_mut().filter_map(Value::as_object_mut) {
                    upgrade_output(output);
                }
            }
        }
        Some("heading") => {
            cell.insert("cell_type".into(), "markdown".into());
            let level = cell.remove("level").and_then(|l| l.as_u64()).unwrap_or(1);
            let text = cell.get("source").and_then(Value::as_str).unwrap_or("");
            let source = format!(
                "{} {}",
                "#".repeat(level as usize),
                splitlines_without_ends(text).join(" ")
            );
            cell.insert("source".into(), source.into());
        }
        Some("html") => {
            cell.insert("cell_type".into(), "markdown".into());
        }
        _ => {}
    }
}

/// `v4.convert.upgrade_output`.
fn upgrade_output(output: &mut Map<String, Value>) {
    match output.get("output_type").and_then(Value::as_str) {
        Some(kind @ ("pyout" | "display_data")) => {
            if kind == "pyout" {
                output.insert("output_type".into(), "execute_result".into());
                let count = output.remove("prompt_number").unwrap_or(Value::Null);
                output.insert("execution_count".into(), count);
            }
            output
                .entry("metadata")
                .or_insert_with(|| Value::Object(Map::new()));
            let keys: Vec<String> = output
                .keys()
                .filter(|k| !matches!(k.as_str(), "output_type" | "execution_count" | "metadata"))
                .cloned()
                .collect();
            let mut data = Map::new();
            for key in keys {
                if let Some(value) = output.remove(&key) {
                    data.insert(key, value);
                }
            }
            to_mime_keys(&mut data);
            if let Some(Value::String(text)) = data.get("application/json") {
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    data.insert("application/json".into(), parsed);
                }
            }
            output.insert("data".into(), Value::Object(data));
            if let Some(Value::Object(metadata)) = output.get_mut("metadata") {
                to_mime_keys(metadata);
            }
        }
        Some("pyerr") => {
            output.insert("output_type".into(), "error".into());
        }
        Some("stream") => {
            let name = output.remove("stream").unwrap_or_else(|| "stdout".into());
            output.insert("name".into(), name);
        }
        _ => {}
    }
}

/// `v4.convert.to_mime_key`.
fn to_mime_keys(d: &mut Map<String, Value>) {
    for (alias, mime) in MIME_MAP {
        if let Some(value) = d.remove(alias) {
            d.insert(mime.into(), value);
        }
    }
}

/// Python's `str.splitlines()`: [`splitlines`] without the line endings.
fn splitlines_without_ends(s: &str) -> Vec<String> {
    splitlines(s)
        .into_iter()
        .map(|mut line| {
            if line.ends_with("\r\n") {
                line.truncate(line.len() - 2);
            } else if let Some(c) = line.chars().last().filter(|c| is_boundary(*c)) {
                line.truncate(line.len() - c.len_utf8());
            }
            line
        })
        .collect()
}

fn is_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_matches_python() {
        // Python: "a\r\nb\rc\nd\x0be\u2028f\n".splitlines()
        assert_eq!(
            splitlines_without_ends("a\r\nb\rc\nd\x0be\u{2028}f\n"),
            ["a", "b", "c", "d", "e", "f"]
        );
        assert_eq!(splitlines_without_ends("one"), ["one"]);
        assert_eq!(splitlines_without_ends("a\n\nb"), ["a", "", "b"]);
        assert!(splitlines_without_ends("").is_empty());
        assert_eq!(splitlines_without_ends("\n"), [""]);
    }

    #[test]
    fn join_lines_handles_both_split_styles() {
        let mut obj = serde_json::json!({
            "kept": ["a\n", "b"],
            "joined": ["a", "b"],
            "empty": [],
            "mixed": ["a", 1],
        });
        let obj = obj.as_object_mut().unwrap();
        for key in ["kept", "joined", "empty", "mixed"] {
            join_field(obj, key);
        }
        assert_eq!(obj["kept"], "a\nb");
        assert_eq!(obj["joined"], "a\nb");
        assert_eq!(obj["empty"], "");
        // Not all strings: left for the v4 reader to report.
        assert_eq!(obj["mixed"], serde_json::json!(["a", 1]));
    }
}
