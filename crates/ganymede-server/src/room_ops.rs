//! Cell edits on a notebook room, shared by the MCP tools ([`crate::mcp`]),
//! `ganymede execute`'s runs ([`crate::execute`]) and the host API's
//! `self.*` calls ([`crate::host`]) (#768), so the three can't drift:
//! adding a cell, replacing its source (with provenance and history),
//! deleting it, and reading its source.
//!
//! Each works inside one `Room::edit` transaction. Checks that differ by
//! caller (MCP's read marks, `self.*`'s source hash) stay with the caller.

use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::{Any, Array, GetString, Map, MapRef, Out, ReadTxn, TransactionMut};
use ganymede_documents::Room;
use serde_json::{json, Value};

/// Why an edit is refused on a room another server owns (#272): its room
/// would undo the edit and never save it.
pub(crate) const READ_ONLY: &str = "the notebook is open in another server on this machine";

/// Whether `room` takes edits: false when another server owns its file.
pub(crate) fn writable(room: &Room) -> bool {
    !room.is_read_only()
}

/// How many superseded sources a cell's `metadata.ganymede.agent.history`
/// keeps; older ones are dropped.
pub const HISTORY_LIMIT: usize = 10;

/// Now, as provenance records it.
pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A new cell's JSON: `cell_type` is `code`, `markdown` or `raw`; `None`
/// for any other.
pub(crate) fn new_cell(cell_type: &str, id: &str, source: &str, metadata: Value) -> Option<Value> {
    match cell_type {
        "code" => Some(
            json!({"cell_type": "code", "execution_count": null, "id": id,
                              "metadata": metadata, "outputs": [], "source": source}),
        ),
        "markdown" | "raw" => {
            Some(json!({"cell_type": cell_type, "id": id, "metadata": metadata, "source": source}))
        }
        _ => None,
    }
}

/// A cell's source as one string, however the room stores it: shared
/// text, a string, or a list of strings. `None` for anything else.
pub(crate) fn source_of<T: ReadTxn>(txn: &T, cell: &MapRef) -> Option<String> {
    match cell.get(txn, "source")? {
        Out::YText(t) => Some(t.get_string(txn)),
        Out::Any(Any::String(s)) => Some(s.to_string()),
        other => match schema::out_to_json(txn, &other) {
            Value::String(s) => Some(s),
            Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect()),
            _ => None,
        },
    }
}

/// Insert `cell` after cell `after`, or at the end. `Err` names an `after`
/// that isn't there; nothing changes then.
pub(crate) fn insert(
    txn: &mut TransactionMut,
    roots: &Roots,
    cell: Value,
    after: Option<&str>,
) -> Result<(), String> {
    let at = match after {
        None => roots.cells.len(txn),
        Some(a) => schema::cell_index(txn, roots, a)
            .map(|i| i + 1)
            .ok_or_else(|| a.to_string())?,
    };
    schema::insert_cell(txn, &roots.cells, at, cell);
    Ok(())
}

/// Replace cell `id`'s source with `source`, as `by`. A changed source is
/// stamped with provenance, and the source it replaced, unless empty, is
/// kept in its history ([`stamp`]); an unchanged one is no edit and keeps
/// the cell's provenance. False when there is no cell `id`.
pub(crate) fn set_source(
    txn: &mut TransactionMut,
    roots: &Roots,
    id: &str,
    source: &str,
    by: &str,
) -> bool {
    let Some(cell) = schema::find_cell(txn, roots, id) else {
        return false;
    };
    let previous = source_of(txn, &cell);
    if previous.as_deref() == Some(source) {
        return true;
    }
    match cell.get(txn, "source") {
        Some(Out::YText(t)) => schema::set_text(txn, &t, source),
        _ => {
            cell.insert(
                txn,
                "source",
                ganymede_documents::yrs::TextPrelim::new(source.to_string()),
            );
        }
    }
    stamp(txn, &cell, by, previous.filter(|p| !p.is_empty()));
    true
}

/// Delete cell `id`. False when there is none.
pub(crate) fn delete(txn: &mut TransactionMut, roots: &Roots, id: &str) -> bool {
    match schema::cell_index(txn, roots, id) {
        Some(i) => {
            roots.cells.remove(txn, i);
            true
        }
        None => false,
    }
}

/// Update an existing cell's provenance after an edit: `at`, and `by` when
/// the cell had none. A `superseded` source is appended to `history` as
/// `{"source", "by", "at"}`: the text the edit replaced, who replaced it
/// and when.
pub(crate) fn stamp(txn: &mut TransactionMut, cell: &MapRef, by: &str, superseded: Option<String>) {
    let meta = match cell.get(txn, "metadata") {
        Some(Out::YMap(m)) => m,
        _ => cell.insert(
            txn,
            "metadata",
            ganymede_documents::yrs::MapPrelim::default(),
        ),
    };
    let mut g = meta
        .get(txn, "ganymede")
        .map(|o| schema::out_to_json(txn, &o))
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    // Indexing a non-object panics; another tool may have written one.
    if !g["agent"].is_object() {
        g["agent"] = json!({});
    }
    let agent = &mut g["agent"];
    if agent.get("by").is_none() {
        agent["by"] = json!(by);
        agent["mode"] = json!("note");
    }
    let at = now();
    if let Some(source) = superseded {
        let mut history = match agent["history"].take() {
            Value::Array(h) => h,
            _ => Vec::new(),
        };
        history.push(json!({"source": source, "by": by, "at": at}));
        let excess = history.len().saturating_sub(HISTORY_LIMIT);
        history.drain(..excess);
        agent["history"] = Value::Array(history);
    }
    agent["at"] = json!(at);
    meta.insert(txn, "ganymede", schema::json_to_any(&g));
}
