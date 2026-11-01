//! A notebook as a Y document, in the schema `jupyter_ydoc`'s `YNotebook`
//! (Python) and `@jupyter/ydoc`'s `YNotebook` (JupyterLab) share:
//!
//! ```text
//! state: Map    dirty, path, hash, document_id
//! meta:  Map    nbformat, nbformat_minor, metadata: Map, and any other
//!               top-level notebook fields
//! cells: Array of Map
//!          id, cell_type, source: Text, metadata: Map,
//!          code:  execution_count, execution_state, outputs: Array of Map
//!                 (a stream output's text is a Text)
//!          markdown/raw: attachments (only when not empty)
//! ```
//!
//! Values below those shared types are plain JSON (`Any`).
//!
//! Differences from `jupyter_ydoc`, all so that a notebook that goes in
//! comes out byte for byte as `ganymede-nbformat` writes it:
//!
//! - No `language_info` or `kernelspec` defaults are added to metadata.
//! - Numbers keep their kind: integers are written as Yjs integers and
//!   floats as floats, where `jupyter_ydoc` turns every top-level number
//!   into a float and back into an integer (`cast_all`). Yjs encodes an
//!   integer outside the 32-bit range as a float, so an integral float
//!   outside that range is read back as an integer.
//! - Top-level fields other than `cells`, `metadata`, `nbformat` and
//!   `nbformat_minor` are kept in `meta`, which `jupyter_ydoc` drops.

use ganymede_nbformat::Notebook;
use serde_json::{Map as JsonMap, Number as JsonNumber, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use yrs::types::ToJson;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, Out, ReadTxn, Text,
    TextPrelim, TextRef, Transact, TransactionMut,
};

/// Root type names.
pub const CELLS: &str = "cells";
pub const META: &str = "meta";
pub const STATE: &str = "state";

/// Largest integer Yjs encodes as an integer (`lib0`'s `writeAny` uses
/// a variable-length integer within 32 bits and a float outside).
const YJS_INT_MAX: f64 = 0x7FFF_FFFF as f64;

/// Errors turning a Y document back into a notebook.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("the document is not a notebook: {0}")]
    NotANotebook(#[from] ganymede_nbformat::Error),
}

/// The root types of a notebook document.
#[derive(Clone)]
pub struct Roots {
    pub cells: ArrayRef,
    pub meta: MapRef,
    pub state: MapRef,
}

impl Roots {
    pub fn get(doc: &Doc) -> Self {
        Roots {
            cells: doc.get_or_insert_array(CELLS),
            meta: doc.get_or_insert_map(META),
            state: doc.get_or_insert_map(STATE),
        }
    }
}

/// Build a fresh document holding `notebook`, with client id 0 and so the
/// same history every time for the same notebook. `jupyter-collaboration`
/// does the same, so that a client that reconnects to a room rebuilt from
/// an unchanged file merges cleanly instead of duplicating content.
pub fn deterministic_update(notebook: &Notebook) -> Vec<u8> {
    let doc = Doc::with_client_id(0);
    let roots = Roots::get(&doc);
    {
        let mut txn = doc.transact_mut();
        set(&mut txn, &roots, notebook);
    }
    let txn = doc.transact();
    txn.encode_state_as_update_v1(&yrs::StateVector::default())
}

/// Replace the document's content with `notebook`, granularly: a cell
/// whose id is still there and whose content is unchanged is left alone,
/// one whose source alone changed gets a text edit, and the rest are
/// replaced. The `state` map is not touched.
pub fn set(txn: &mut TransactionMut, roots: &Roots, notebook: &Notebook) {
    let value = notebook_value(notebook);
    let Value::Object(mut top) = value else {
        unreachable!("a notebook serializes to an object")
    };
    let cells = match top.remove("cells") {
        Some(Value::Array(cells)) => cells,
        _ => Vec::new(),
    };
    set_meta(txn, &roots.meta, top);
    set_cells(txn, &roots.cells, cells);
}

/// The notebook as the JSON tree this module stores: `serde`'s in-memory
/// form, with multi-line strings joined.
pub fn notebook_value(notebook: &Notebook) -> Value {
    serde_json::to_value(notebook).expect("a notebook serializes to JSON")
}

fn set_meta(txn: &mut TransactionMut, meta: &MapRef, top: JsonMap<String, Value>) {
    let wanted: HashSet<&str> = top.keys().map(String::as_str).collect();
    let stale: Vec<String> = meta
        .keys(txn)
        .filter(|k| !wanted.contains(k))
        .map(str::to_string)
        .collect();
    for key in stale {
        meta.remove(txn, &key);
    }
    for (key, value) in sorted(top) {
        let current = meta.get(txn, &key).map(|o| out_to_json(txn, &o));
        if current.as_ref() == Some(&through_yjs(&value)) {
            continue;
        }
        if key == "metadata" {
            let obj = match value {
                Value::Object(m) => m,
                _ => JsonMap::new(),
            };
            let ymeta = meta.insert(txn, key, MapPrelim::default());
            fill_shallow(txn, &ymeta, obj);
        } else {
            meta.insert(txn, key, json_to_any(&value));
        }
    }
}

fn set_cells(txn: &mut TransactionMut, ycells: &ArrayRef, cells: Vec<Value>) {
    // Current cells by id, first occurrence only.
    let mut old: HashMap<String, Value> = HashMap::new();
    for out in ycells.iter(txn) {
        let cell = out_to_json(txn, &out);
        if let Some(id) = cell.get("id").and_then(Value::as_str) {
            old.entry(id.to_string())
                .or_insert_with(|| strip_runtime(cell.clone()));
        }
    }
    // Which old cells survive as they are, or with a source edit.
    let mut keep: HashSet<String> = HashSet::new();
    let mut source_only: HashMap<String, String> = HashMap::new();
    for cell in &cells {
        let Some(id) = cell.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(prev) = old.get(id) else { continue };
        if keep.contains(id) {
            continue; // a duplicate id in the new list is inserted fresh
        }
        let cell_y = through_yjs(cell);
        if *prev == cell_y {
            keep.insert(id.to_string());
        } else if same_except_source(prev, &cell_y) {
            keep.insert(id.to_string());
            let src = cell.get("source").and_then(Value::as_str).unwrap_or("");
            source_only.insert(id.to_string(), src.to_string());
        }
    }
    // Delete cells not kept, and duplicates.
    let mut seen = HashSet::new();
    let mut i = 0;
    while i < ycells.len(txn) {
        let id = cell_id(txn, ycells, i);
        let retained = id
            .as_ref()
            .is_some_and(|id| keep.contains(id) && seen.insert(id.clone()));
        if retained {
            i += 1;
        } else {
            ycells.remove(txn, i);
        }
    }
    // Put cells in order, inserting new ones and moving kept ones
    // (by delete and re-insert, as yjs 13 has no move).
    for (index, cell) in cells.into_iter().enumerate() {
        let id = cell.get("id").and_then(Value::as_str).map(str::to_string);
        let here = (index as u32) < ycells.len(txn) && cell_id(txn, ycells, index as u32) == id;
        if here {
            if let Some(src) = id.as_ref().and_then(|id| source_only.get(id)) {
                if let Some(Out::YMap(ycell)) = ycells.get(txn, index as u32) {
                    if let Some(Out::YText(text)) = ycell.get(txn, "source") {
                        set_text(txn, &text, src);
                    }
                }
            }
            continue;
        }
        if let Some(id) = &id {
            if keep.contains(id) {
                let len = ycells.len(txn);
                if let Some(pos) = (index as u32 + 1..len)
                    .find(|&p| cell_id(txn, ycells, p).as_deref() == Some(id.as_str()))
                {
                    ycells.remove(txn, pos);
                }
            }
        }
        insert_cell(txn, ycells, index as u32, cell);
    }
}

fn cell_id<T: ReadTxn>(txn: &T, ycells: &ArrayRef, index: u32) -> Option<String> {
    match ycells.get(txn, index)? {
        Out::YMap(m) => match m.get(txn, "id")? {
            Out::Any(Any::String(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn same_except_source(a: &Value, b: &Value) -> bool {
    let (Value::Object(a), Value::Object(b)) = (a, b) else {
        return false;
    };
    let strip = |m: &JsonMap<String, Value>| {
        let mut m = m.clone();
        m.remove("source");
        m
    };
    strip(a) == strip(b) && b.get("source").is_some_and(Value::is_string)
}

/// Replace a text's content with the smallest edit at one place: keep the
/// common prefix and suffix, so cursors elsewhere in the text stay put.
pub fn set_text(txn: &mut TransactionMut, text: &TextRef, new: &str) {
    let old = text.get_string(txn);
    if old == new {
        return;
    }
    // Offsets are UTF-8 bytes, the default `OffsetKind` of every document
    // this crate makes; prefix and suffix end on char boundaries.
    let prefix = old
        .char_indices()
        .zip(new.chars())
        .take_while(|((_, a), b)| a == b)
        .last()
        .map(|((i, c), _)| i + c.len_utf8())
        .unwrap_or(0);
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = suffix.min(old_rest.len()).min(new_rest.len());
    let del = old_rest.len() - suffix;
    let ins = &new_rest[..new_rest.len() - suffix];
    let at = prefix as u32;
    if del > 0 {
        text.remove_range(txn, at, del as u32);
    }
    if !ins.is_empty() {
        text.insert(txn, at, ins);
    }
}

/// Insert a cell as a map in the schema above. Keys go in sorted order so
/// that the same cell always gets the same history.
fn insert_cell(txn: &mut TransactionMut, ycells: &ArrayRef, index: u32, cell: Value) {
    let Value::Object(mut cell) = cell else {
        // Not an object: nothing sensible to show, keep it as data.
        ycells.insert(txn, index, json_to_any(&cell));
        return;
    };
    let is_code = cell.get("cell_type").and_then(Value::as_str) == Some("code");
    if !cell.contains_key("id") {
        cell.insert("id".into(), Value::String(uuid::Uuid::new_v4().to_string()));
    }
    if is_code {
        cell.insert("execution_state".into(), Value::String("idle".into()));
    }
    let ycell = ycells.insert(txn, index, MapPrelim::default());
    for (key, value) in sorted(cell) {
        match (key.as_str(), value) {
            ("source", Value::String(s)) => {
                ycell.insert(txn, key, TextPrelim::new(s));
            }
            ("metadata", Value::Object(m)) => {
                let ymeta = ycell.insert(txn, key, MapPrelim::default());
                fill_shallow(txn, &ymeta, m);
            }
            ("outputs", Value::Array(outputs)) if is_code => {
                let youtputs = ycell.insert(txn, key, ArrayPrelim::default());
                for (i, output) in outputs.into_iter().enumerate() {
                    insert_output(txn, &youtputs, i as u32, output);
                }
            }
            (_, value) => {
                ycell.insert(txn, key, json_to_any(&value));
            }
        }
    }
}

/// Insert an output as a map; a stream's text is a Text so it can grow in
/// place.
pub fn insert_output(txn: &mut TransactionMut, youtputs: &ArrayRef, index: u32, output: Value) {
    let Value::Object(output) = output else {
        youtputs.insert(txn, index, json_to_any(&output));
        return;
    };
    let is_stream = output.get("output_type").and_then(Value::as_str) == Some("stream");
    let youtput = youtputs.insert(txn, index, MapPrelim::default());
    for (key, value) in sorted(output) {
        match (key.as_str(), value) {
            ("text", Value::String(s)) if is_stream => {
                youtput.insert(txn, key, TextPrelim::new(s));
            }
            (_, value) => {
                youtput.insert(txn, key, json_to_any(&value));
            }
        }
    }
}

/// Fill a map whose values are plain JSON.
fn fill_shallow(txn: &mut TransactionMut, map: &MapRef, m: JsonMap<String, Value>) {
    for (key, value) in sorted(m) {
        map.insert(txn, key, json_to_any(&value));
    }
}

fn sorted(m: JsonMap<String, Value>) -> Vec<(String, Value)> {
    let mut entries: Vec<(String, Value)> = m.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Read the document back as a notebook.
pub fn get<T: ReadTxn>(txn: &T, roots: &Roots) -> Result<Notebook, SchemaError> {
    get_restoring(txn, roots, None)
}

/// [`get`], with numbers Yjs could not hold put back from `baseline`, the
/// notebook as last loaded (see [`restore_numbers`]).
pub fn get_restoring<T: ReadTxn>(
    txn: &T,
    roots: &Roots,
    baseline: Option<&Value>,
) -> Result<Notebook, SchemaError> {
    let mut top = match out_to_json(txn, &Out::YMap(roots.meta.clone())) {
        Value::Object(m) => m,
        _ => JsonMap::new(),
    };
    let minor = top
        .get("nbformat_minor")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let major = top.get("nbformat").and_then(Value::as_u64).unwrap_or(0);
    let strip_ids = major == 4 && minor <= 4;
    let cells: Vec<Value> = roots
        .cells
        .iter(txn)
        .map(|out| {
            let mut cell = strip_runtime(out_to_json(txn, &out));
            if strip_ids {
                if let Value::Object(m) = &mut cell {
                    m.remove("id");
                }
            }
            cell
        })
        .collect();
    top.insert("cells".into(), Value::Array(cells));
    let mut value = Value::Object(top);
    if let Some(old) = baseline {
        value = restore_numbers(value, old);
    }
    Ok(Notebook::from_value(value)?)
}

/// Drop what the schema adds that is not part of nbformat.
fn strip_runtime(mut cell: Value) -> Value {
    if let Value::Object(m) = &mut cell {
        m.remove("execution_state");
        let empty_attachments = m
            .get("attachments")
            .is_some_and(|a| a.as_object().is_some_and(JsonMap::is_empty));
        if empty_attachments && m.get("cell_type").and_then(Value::as_str) != Some("code") {
            m.remove("attachments");
        }
    }
    cell
}

/// A shared value as JSON, with numbers as described in the module docs.
pub fn out_to_json<T: ReadTxn>(txn: &T, out: &Out) -> Value {
    match out {
        Out::Any(any) => any_to_json(any),
        Out::YText(t) => Value::String(t.get_string(txn)),
        Out::YArray(a) => Value::Array(a.iter(txn).map(|o| out_to_json(txn, &o)).collect()),
        Out::YMap(m) => Value::Object(
            m.iter(txn)
                .map(|(k, v)| (k.to_string(), out_to_json(txn, &v)))
                .collect(),
        ),
        other => any_to_json(&other.to_json(txn)),
    }
}

pub fn any_to_json(any: &Any) -> Value {
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(n) => number_to_json(*n),
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(b) => Value::Array(b.iter().map(|x| Value::from(*x)).collect()),
        Any::Array(items) => Value::Array(items.iter().map(any_to_json).collect()),
        Any::Map(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), any_to_json(v))).collect()),
    }
}

fn number_to_json(n: yrs::any::Number) -> Value {
    use yrs::any::Number;
    match n {
        Number::Int(i) => Value::from(i),
        Number::Float(f) if f.fract() == 0.0 && f.abs() > YJS_INT_MAX && f.abs() < 9.007e15 => {
            Value::from(f as i64)
        }
        Number::Float(f) => JsonNumber::from_f64(f).map_or(Value::Null, Value::Number),
    }
}

pub fn json_to_any(value: &Value) -> Any {
    use yrs::any::Number;
    match value {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => {
            // Beyond 2^53 yrs writes a BigInt, which JavaScript reads
            // as `bigint` and JSON cannot hold; a float is what JSON.parse
            // would make of it anyway.
            if let Some(i) = n.as_i64().filter(|i| i.unsigned_abs() < 1 << 53) {
                Any::Number(Number::Int(i))
            } else if let Some(u) = n.as_u64() {
                Any::Number(Number::Float(u as f64))
            } else {
                Any::Number(Number::Float(n.as_f64().unwrap_or(f64::NAN)))
            }
        }
        Value::String(s) => Any::String(Arc::from(s.as_str())),
        Value::Array(items) => Any::Array(items.iter().map(json_to_any).collect()),
        Value::Object(m) => Any::Map(Arc::new(
            m.iter().map(|(k, v)| (k.clone(), json_to_any(v))).collect(),
        )),
    }
}

/// A JSON value as it comes back after a trip through a Yjs document:
/// numbers as `lib0` encodes and decodes them.
pub fn through_yjs(value: &Value) -> Value {
    use yrs::encoding::read::Cursor;
    use yrs::updates::encoder::{Encoder, EncoderV1};
    let any = json_to_any(value);
    let mut enc = EncoderV1::new();
    any.encode(&mut enc);
    let bytes = enc.to_vec();
    match Any::decode(&mut Cursor::new(&bytes)) {
        Ok(any) => any_to_json(&any),
        Err(_) => value.clone(),
    }
}

/// Put back numbers that Yjs could not hold exactly: where `new` has the
/// value a number in `old` turns into in a Y document, take `old`'s. So a
/// notebook with integers beyond 2^53, or integral floats beyond 2^31,
/// saves byte for byte unless someone changed those values. Arrays of
/// objects with `id`s (cells) are matched by id, other arrays by index,
/// objects by key.
pub fn restore_numbers(new: Value, old: &Value) -> Value {
    match (new, old) {
        (Value::Number(n), Value::Number(o)) => {
            let n = Value::Number(n);
            if n != *old && through_yjs(old) == n {
                Value::Number(o.clone())
            } else {
                n
            }
        }
        (Value::Object(n), Value::Object(o)) => Value::Object(
            n.into_iter()
                .map(|(k, v)| {
                    let v = match o.get(&k) {
                        Some(ov) => restore_numbers(v, ov),
                        None => v,
                    };
                    (k, v)
                })
                .collect(),
        ),
        (Value::Array(n), Value::Array(o)) => {
            let ids: HashMap<&str, &Value> = o
                .iter()
                .filter_map(|v| Some((v.get("id")?.as_str()?, v)))
                .collect();
            Value::Array(
                n.into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let by_id = v
                            .get("id")
                            .and_then(Value::as_str)
                            .and_then(|id| ids.get(id).copied());
                        match by_id.or_else(|| if ids.is_empty() { o.get(i) } else { None }) {
                            Some(ov) => restore_numbers(v, ov),
                            None => v,
                        }
                    })
                    .collect(),
            )
        }
        (new, _) => new,
    }
}
