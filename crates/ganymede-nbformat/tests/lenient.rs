//! Lenient reading (#73): notebooks Python nbformat reads, with logged
//! validation errors, must read here too, and must not lose data.
//!
//! The fixtures come from `tests/fixtures/generate.py`.

use std::path::{Path, PathBuf};

use ganymede_nbformat::{Cell, Notebook, Output};
use serde_json::{json, Value};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn is_generated_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Outputs that break the schema are kept as raw JSON and written back
/// exactly as `nbformat.write` wrote them.
#[test]
fn invalid_outputs_roundtrip_is_byte_identical() {
    let original = read_fixture("invalid_outputs.ipynb");
    let nb = Notebook::from_str(&original).unwrap();
    let written = nb.to_file_string();
    assert!(
        written == original,
        "--- nbformat\n{original}\n--- rust\n{written}"
    );

    let Cell::Code(cell) = &nb.cells[0] else {
        panic!()
    };
    let raw: Vec<&Value> = cell.outputs[..4]
        .iter()
        .map(|o| match o {
            Output::Unknown(v) => v,
            other => panic!("expected a raw output, got {other:?}"),
        })
        .collect();
    assert_eq!(raw[0]["payload"], json!({"b": 1, "a": [1, 2]}));
    assert_eq!(cell.outputs[0].output_type(), Some("future_output"));
    // Kept as read: the lines are not rejoined, which does not matter
    // because the writer splits them again.
    assert_eq!(raw[1]["name"], 5);
    assert_eq!(raw[2]["metadata"], "not a map");
    assert_eq!(raw[3]["traceback"], "not a list");
    assert!(matches!(&cell.outputs[4], Output::Stream(s) if s.text == "valid\n"));
}

#[test]
fn lenient_fixture_reads_like_nbformat() {
    let nb = Notebook::from_str(&read_fixture("lenient.ipynb")).unwrap();

    // Cell ids: the missing one and the second `dup` are replaced, as
    // nbformat's `_normalize` does; the first `dup` and the rest are kept.
    let ids: Vec<&str> = nb.cells.iter().map(|c| c.id().unwrap()).collect();
    assert!(is_generated_id(ids[0]), "{ids:?}");
    assert_eq!(ids[1], "dup");
    assert!(is_generated_id(ids[2]), "{ids:?}");
    assert_eq!(&ids[3..], ["plain", "surrogates"]);
    assert_ne!(ids[0], ids[2]);

    // `source: null` reads as empty.
    assert_eq!(nb.cells[0].source().as_str(), "");

    let Cell::Code(first) = &nb.cells[1] else {
        panic!()
    };
    // `1.0` is the integer 1.
    assert_eq!(first.execution_count, Some(1));
    // A stream without `name` defaults to stdout, as `new_output` does.
    let Output::Stream(stream) = &first.outputs[0] else {
        panic!("{:?}", first.outputs[0])
    };
    assert_eq!(stream.name, "stdout");
    assert_eq!(stream.text, "no name\n");
    // An error without `ename`/`evalue` gets empty strings.
    let Output::Error(error) = &first.outputs[1] else {
        panic!("{:?}", first.outputs[1])
    };
    assert_eq!((error.ename.as_str(), error.evalue.as_str()), ("", ""));
    // `execution_count: -1` reads as not executed, `metadata: null` as empty.
    let Output::ExecuteResult(result) = &first.outputs[2] else {
        panic!("{:?}", first.outputs[2])
    };
    assert_eq!(result.execution_count, None);
    assert!(result.metadata.is_empty());
    assert_eq!(result.data.get_str("text/plain"), Some("1"));

    let Cell::Code(second) = &nb.cells[2] else {
        panic!()
    };
    assert_eq!(second.execution_count, None);
    assert_eq!(second.source, "x = 1");

    // Lone surrogates become U+FFFD; a valid pair and an escaped backslash
    // are untouched.
    assert_eq!(
        nb.cells[4].source().as_str(),
        "lone \u{fffd} and \u{fffd}, pair 🚀, literal \\ud800"
    );
}

/// What the lenient reader repaired is written in valid form, and reading
/// that back gives the same notebook: nothing changes on a second save.
#[test]
fn lenient_fixture_writes_valid_json_that_roundtrips() {
    let nb = Notebook::from_str(&read_fixture("lenient.ipynb")).unwrap();
    let written = nb.to_file_string();
    let value: Value = serde_json::from_str(&written).unwrap();
    let cells = value["cells"].as_array().unwrap();
    assert_eq!(cells[0]["source"], json!([]));
    assert_eq!(cells[1]["execution_count"], 1);
    assert_eq!(cells[1]["outputs"][0]["name"], "stdout");
    assert_eq!(cells[1]["outputs"][1]["ename"], "");
    assert_eq!(cells[1]["outputs"][2]["execution_count"], Value::Null);
    assert_eq!(cells[1]["outputs"][2]["metadata"], json!({}));
    assert_eq!(cells[2]["execution_count"], Value::Null);
    assert_eq!(cells[2]["id"], nb.cells[2].id().unwrap());

    let again = Notebook::from_str(&written).unwrap();
    assert_eq!(again, nb);
    assert_eq!(again.to_file_string(), written);
}

#[test]
fn nulls_read_as_empty() {
    let nb = Notebook::from_value(json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": null,
        "cells": [
            {"cell_type": "code", "id": "c", "metadata": null, "source": null,
             "execution_count": null, "outputs": null},
            {"cell_type": "raw", "id": "r", "metadata": null, "source": null, "attachments": null},
        ],
    }))
    .unwrap();
    assert!(nb.metadata.is_empty());
    let Cell::Code(code) = &nb.cells[0] else {
        panic!()
    };
    assert!(code.metadata.is_empty());
    assert!(code.outputs.is_empty());
    assert_eq!(code.source, "");
    let Cell::Raw(raw) = &nb.cells[1] else {
        panic!()
    };
    assert_eq!(raw.attachments, None);

    let null_cells =
        Notebook::from_value(json!({"nbformat": 4, "nbformat_minor": 5, "cells": null}));
    assert!(null_cells.unwrap().cells.is_empty());
}

#[test]
fn execution_counts_read_leniently() {
    let read = |count: Value| {
        let nb = Notebook::from_value(json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [{"cell_type": "code", "id": "c", "metadata": {}, "source": "",
                       "outputs": [], "execution_count": count}],
        }))
        .unwrap();
        let Cell::Code(c) = &nb.cells[0] else {
            panic!()
        };
        c.execution_count
    };
    assert_eq!(read(json!(3)), Some(3));
    assert_eq!(read(json!(3.0)), Some(3));
    assert_eq!(read(json!(0.0)), Some(0));
    assert_eq!(read(json!(-1)), None);
    assert_eq!(read(json!(-1.0)), None);
    assert_eq!(read(json!(1.5)), None);
    assert_eq!(read(json!("3")), None);
    assert_eq!(read(json!(true)), None);
    assert_eq!(read(Value::Null), None);
}

#[test]
fn outputs_without_a_type_are_kept() {
    let nb = Notebook::from_value(json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [{"cell_type": "code", "id": "c", "metadata": {}, "source": "",
                   "execution_count": null,
                   "outputs": [{"no_output_type": true}, 42, null, {"output_type": 7}]}],
    }))
    .unwrap();
    let Cell::Code(c) = &nb.cells[0] else {
        panic!()
    };
    assert_eq!(
        c.outputs,
        vec![
            Output::Unknown(json!({"no_output_type": true})),
            Output::Unknown(json!(42)),
            Output::Unknown(Value::Null),
            Output::Unknown(json!({"output_type": 7})),
        ]
    );
    assert_eq!(c.outputs[3].output_type(), None);
    let back = nb.to_disk_value();
    assert_eq!(
        back["cells"][0]["outputs"],
        json!([{"no_output_type": true}, 42, null, {"output_type": 7}])
    );
}

/// Cell ids are filled and deduplicated on write too, as `nbformat.write`
/// does, so a notebook built in memory without ids is saved valid.
#[test]
fn write_repairs_cell_ids_for_minor_5() {
    let mut nb = Notebook::new();
    for id in [None, Some("a"), Some("a"), None] {
        nb.cells
            .push(Cell::Markdown(ganymede_nbformat::MarkdownCell {
                id: id.map(str::to_owned),
                ..Default::default()
            }));
    }
    let value = nb.to_disk_value();
    let ids: Vec<&str> = value["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(is_generated_id(ids[0]));
    assert_eq!(ids[1], "a");
    assert!(is_generated_id(ids[2]));
    assert!(is_generated_id(ids[3]));
    let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 4);
}

/// Before nbformat 4.5 cells have no ids: nothing is added or changed.
#[test]
fn minor_4_cell_ids_are_left_alone() {
    let text = r#"{"nbformat": 4, "nbformat_minor": 4, "metadata": {}, "cells": [
        {"cell_type": "markdown", "metadata": {}, "source": "a"},
        {"cell_type": "markdown", "id": "x", "metadata": {}, "source": "b"},
        {"cell_type": "markdown", "id": "x", "metadata": {}, "source": "c"}]}"#;
    let nb = Notebook::from_str(text).unwrap();
    let ids: Vec<Option<&str>> = nb.cells.iter().map(Cell::id).collect();
    assert_eq!(ids, [None, Some("x"), Some("x")]);
    let value = nb.to_disk_value();
    assert!(value["cells"][0].get("id").is_none());
    assert_eq!(value["cells"][2]["id"], "x");
}
