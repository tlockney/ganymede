//! Writing notebooks: output identical to Python nbformat's JSON writer.
//!
//! nbformat 5.x writes `json.dumps(nb, indent=1, sort_keys=True,
//! separators=(",", ": "), ensure_ascii=False)` after `split_lines` and
//! `strip_transient`, and `nbformat.write` appends a newline if the text does
//! not already end with one. This module reproduces each step on the JSON
//! tree.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::ser::{Formatter, PrettyFormatter};
use serde_json::{Map, Value};

use crate::multiline::splitlines;
use crate::reader::strip_transient;
use crate::types::{is_split_mime, Notebook};
use crate::Result;

fn split_string(value: &mut Value) {
    if let Value::String(s) = value {
        let lines = splitlines(s).into_iter().map(Value::String).collect();
        *value = Value::Array(lines);
    }
}

/// nbformat's `_split_mimebundle`: split string values under text-like keys.
fn split_mimebundle(data: &mut Map<String, Value>) {
    for (mime, value) in data.iter_mut() {
        if is_split_mime(mime) {
            split_string(value);
        }
    }
}

/// nbformat's `split_lines`, applied to the serialized tree.
///
/// Walks the tree generically rather than the typed model so that, like
/// Python, it also splits an `attachments` map on a cell type where this
/// crate does not model one.
pub(crate) fn split_lines(nb: &mut Value) {
    let Some(Value::Array(cells)) = nb.get_mut("cells") else {
        return;
    };
    for cell in cells {
        let Some(cell) = cell.as_object_mut() else {
            continue;
        };
        if let Some(source) = cell.get_mut("source") {
            split_string(source);
        }
        if let Some(Value::Object(attachments)) = cell.get_mut("attachments") {
            for bundle in attachments.values_mut() {
                if let Value::Object(bundle) = bundle {
                    split_mimebundle(bundle);
                }
            }
        }
        if cell.get("cell_type").and_then(Value::as_str) != Some("code") {
            continue;
        }
        let Some(Value::Array(outputs)) = cell.get_mut("outputs") else {
            continue;
        };
        for output in outputs {
            let Some(output) = output.as_object_mut() else {
                continue;
            };
            match output.get("output_type").and_then(Value::as_str) {
                Some("execute_result") | Some("display_data") => {
                    if let Some(Value::Object(data)) = output.get_mut("data") {
                        split_mimebundle(data);
                    }
                }
                Some("stream") => {
                    if let Some(text) = output.get_mut("text") {
                        split_string(text);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Sort every object's keys by code point, recursively (`sort_keys=True`).
///
/// Rust's `str` ordering is byte order over UTF-8, which equals code point
/// order, which is how Python compares `str` keys.
pub(crate) fn sort_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (key, mut child) in entries {
                sort_keys(&mut child);
                map.insert(key, child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(sort_keys),
        _ => {}
    }
}

/// Format a finite float the way Python's `float.__repr__` (and so
/// `json.dumps`) does.
///
/// Both Python and Rust pick the shortest digit string that round-trips; they
/// differ only in layout. Python uses positional notation when the decimal
/// exponent is in `-4..16` and otherwise scientific notation with a signed,
/// at least two digit exponent (`1e-05`, `1e+16`), where serde_json would
/// write `0.00001` and `1e16`. Notebook signing hashes numbers in this form
/// too (nbformat's `yield_everything` uses `str()`).
pub fn python_float_repr(f: f64) -> String {
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci
        .split_once('e')
        .expect("{:e} always contains an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();

    let mut out = String::new();
    if f.is_sign_negative() {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        // Positional: the value is 0.<digits> * 10^decpt.
        let decpt = exp + 1;
        if decpt <= 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat(decpt.unsigned_abs() as usize));
            out.push_str(&digits);
        } else {
            let decpt = decpt as usize;
            if decpt >= digits.len() {
                out.push_str(&digits);
                out.push_str(&"0".repeat(decpt - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..decpt]);
                out.push('.');
                out.push_str(&digits[decpt..]);
            }
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        if exp.abs() < 10 {
            out.push('0');
        }
        out.push_str(&exp.abs().to_string());
    }
    out
}

/// serde_json's pretty formatter with Python's float layout.
struct PythonFormatter<'a>(PrettyFormatter<'a>);

impl Formatter for PythonFormatter<'_> {
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> std::io::Result<()> {
        writer.write_all(python_float_repr(value).as_bytes())
    }

    fn write_f32<W: ?Sized + Write>(&mut self, writer: &mut W, value: f32) -> std::io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }

    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_array(writer)
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_array(writer)
    }

    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.0.begin_array_value(writer, first)
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(writer)
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_object(writer)
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_object(writer)
    }

    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.0.begin_object_key(writer, first)
    }

    fn end_object_key<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_object_key(writer)
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(writer)
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(writer)
    }
}

/// Serialize a prepared tree like `json.dumps(indent=1, separators=(",", ": "))`.
fn dumps(value: &Value) -> String {
    let mut buf = Vec::new();
    let formatter = PythonFormatter(PrettyFormatter::with_indent(b" "));
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value
        .serialize(&mut ser)
        .expect("serializing a serde_json::Value to a Vec cannot fail");
    String::from_utf8(buf).expect("serde_json writes UTF-8")
}

impl Notebook {
    /// The notebook as a JSON tree in on-disk form: transient metadata
    /// stripped, multi-line text split into arrays of lines, keys sorted.
    pub fn to_disk_value(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("Notebook serializes to a JSON object");
        strip_transient(&mut value);
        split_lines(&mut value);
        sort_keys(&mut value);
        value
    }

    /// The notebook as JSON text, identical to Python `nbformat.writes(nb)`.
    ///
    /// There is no trailing newline, as in `nbformat.writes`; the file
    /// helpers and [`Notebook::to_file_string`] add it as `nbformat.write`
    /// does. `Display` and `to_string()` are the same thing.
    pub fn to_json_string(&self) -> String {
        dumps(&self.to_disk_value())
    }

    /// The notebook as file contents, identical to what `nbformat.write(nb,
    /// path)` writes: the JSON text plus a trailing newline.
    pub fn to_file_string(&self) -> String {
        let mut s = self.to_json_string();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    }

    /// Write the notebook, with a trailing newline, to a writer.
    pub fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        writer.write_all(self.to_file_string().as_bytes())?;
        Ok(())
    }

    /// Write the notebook to a file, replacing any existing contents.
    ///
    /// The bytes are the same as `nbformat.write(nb, path)` produces. This is
    /// a plain write; callers that need atomic replacement should write to a
    /// temporary file and rename it.
    pub fn write_file(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_file_string())?;
        Ok(())
    }
}

impl std::fmt::Display for Notebook {
    /// Formats as nbformat JSON (same as [`Notebook::to_json_string`]).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_json_string())
    }
}

/// Write a notebook file. Same as [`Notebook::write_file`].
pub fn write_file(notebook: &Notebook, path: impl AsRef<Path>) -> Result<()> {
    notebook.write_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_recursively_by_code_point() {
        let mut v = json!({"b": {"z": 1, "a": [{"y": 1, "x": 2}]}, "a": 0, "B": 1, "é": 2, "e": 3});
        sort_keys(&mut v);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["B", "a", "b", "e", "é"]);
        let inner: Vec<&String> = v["b"].as_object().unwrap().keys().collect();
        assert_eq!(inner, ["a", "z"]);
        let deep: Vec<&String> = v["b"]["a"][0].as_object().unwrap().keys().collect();
        assert_eq!(deep, ["x", "y"]);
    }

    #[test]
    fn dumps_matches_python_indent_1() {
        let v = json!({"a": [], "b": {}, "c": [1, {"d": "x\u{1b}\u{7f}é\""}], "e": null, "f": 1.5, "g": true});
        let expected = "{\n \"a\": [],\n \"b\": {},\n \"c\": [\n  1,\n  {\n   \"d\": \"x\\u001b\u{7f}é\\\"\"\n  }\n ],\n \"e\": null,\n \"f\": 1.5,\n \"g\": true\n}";
        assert_eq!(dumps(&v), expected);
    }

    #[test]
    fn floats_match_python_repr() {
        // (value, repr(value)) pairs taken from CPython 3.13.
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (1.5, "1.5"),
            (0.5, "0.5"),
            (123.456, "123.456"),
            (0.0001, "0.0001"),
            (0.00012345, "0.00012345"),
            (1e-5, "1e-05"),
            (2.5e-7, "2.5e-07"),
            (1e-10, "1e-10"),
            (1.5e-300, "1.5e-300"),
            (1e15, "1000000000000000.0"),
            (999999999999999.9, "999999999999999.9"),
            (1e16, "1e+16"),
            (1.2345678901234568e17, "1.2345678901234568e+17"),
            (1e22, "1e+22"),
            (1.5e300, "1.5e+300"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
            (5e-324, "5e-324"),
            (-123456789.125, "-123456789.125"),
            (0.1 + 0.2, "0.30000000000000004"),
        ];
        for (value, expected) in cases {
            assert_eq!(python_float_repr(*value), *expected, "repr({value:?})");
        }
        let v = json!({"a": 1e-5, "b": [1e16, 0.0001], "c": 7});
        assert_eq!(
            dumps(&v),
            "{\n \"a\": 1e-05,\n \"b\": [\n  1e+16,\n  0.0001\n ],\n \"c\": 7\n}"
        );
    }

    #[test]
    fn split_lines_mirrors_nbformat() {
        let mut v = json!({
            "cells": [
                {
                    "cell_type": "code",
                    "source": "a\nb",
                    "attachments": {"f.txt": {"text/plain": "1\n2", "image/png": "AAAA"}},
                    "outputs": [
                        {"output_type": "stream", "text": "o\r\np"},
                        {"output_type": "execute_result", "data": {
                            "text/plain": "x\ny", "application/json": "not\nsplit",
                            "image/svg+xml": "<svg>\n</svg>", "application/javascript": "1;\n2;",
                            "image/png": "iVBOR\nw0KGgo", "application/vnd.x": "a\nb",
                            "text/vnd.x+json": "text/ wins\nover +json"}},
                        {"output_type": "error", "traceback": ["t\nt"]},
                        {"output_type": "custom", "text": "left\nalone"},
                    ]
                },
                {"cell_type": "markdown", "source": "", "outputs": [{"output_type": "stream", "text": "not\ncode"}]},
                {"cell_type": "raw", "source": ["already", "split\n"]},
            ]
        });
        split_lines(&mut v);
        let c = &v["cells"][0];
        assert_eq!(c["source"], json!(["a\n", "b"]));
        assert_eq!(c["attachments"]["f.txt"]["text/plain"], json!(["1\n", "2"]));
        assert_eq!(c["attachments"]["f.txt"]["image/png"], json!("AAAA"));
        assert_eq!(c["outputs"][0]["text"], json!(["o\r\n", "p"]));
        let d = &c["outputs"][1]["data"];
        assert_eq!(d["text/plain"], json!(["x\n", "y"]));
        assert_eq!(d["application/json"], json!("not\nsplit"));
        assert_eq!(d["image/svg+xml"], json!(["<svg>\n", "</svg>"]));
        assert_eq!(d["application/javascript"], json!(["1;\n", "2;"]));
        assert_eq!(d["image/png"], json!("iVBOR\nw0KGgo"));
        assert_eq!(d["application/vnd.x"], json!("a\nb"));
        assert_eq!(d["text/vnd.x+json"], json!(["text/ wins\n", "over +json"]));
        assert_eq!(c["outputs"][2]["traceback"], json!(["t\nt"]));
        assert_eq!(c["outputs"][3]["text"], json!("left\nalone"));
        assert_eq!(v["cells"][1]["source"], json!([]));
        assert_eq!(v["cells"][1]["outputs"][0]["text"], json!("not\ncode"));
        assert_eq!(v["cells"][2]["source"], json!(["already", "split\n"]));
    }

    #[test]
    fn empty_notebook_matches_nbformat() {
        let nb = Notebook::new();
        assert_eq!(
            nb.to_file_string(),
            "{\n \"cells\": [],\n \"metadata\": {},\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}\n"
        );
        assert_eq!(nb.to_string(), nb.to_json_string());
        assert!(!nb.to_json_string().ends_with('\n'));
    }
}
