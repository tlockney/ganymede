//! Named results: values a notebook publishes under a name as outputs, so a
//! run's caller reads them without parsing outputs (#650).
//!
//! Ganymede's own type is [`RESULT_MIME`], holding `{ "name", "value" }`.
//! Python's scrapbook writes [`SCRAPBOOK_JSON_MIME`] and
//! [`SCRAPBOOK_TEXT_MIME`], holding `{ "name", "data", ... }`; those are read
//! too, so notebooks that use `sb.glue` work as they are. The crate knows
//! no kernel language: a result is a MIME type in an output.

use serde_json::{Map, Value};

use crate::{Cell, MimeBundle, Notebook, Output};

/// Ganymede's named-result MIME type: `{ "name": string, "value": JSON }`.
pub const RESULT_MIME: &str = "application/vnd.ganymede.result+json";
/// scrapbook's JSON-encoded scrap: `{ "name", "data", "encoder": "json" }`.
pub const SCRAPBOOK_JSON_MIME: &str = "application/scrapbook.scrap.json+json";
/// scrapbook's text-encoded scrap: `{ "name", "data": string }`.
pub const SCRAPBOOK_TEXT_MIME: &str = "application/scrapbook.scrap.text+json";

/// The named result in one output's MIME bundle, if it has one. Ganymede's
/// type wins over scrapbook's in the same bundle.
pub fn named_result(data: &MimeBundle) -> Option<(String, Value)> {
    let read = |mime: &str, field: &str| {
        let obj = data.get(mime)?.as_object()?;
        let name = obj.get("name")?.as_str().filter(|n| !n.is_empty())?;
        Some((
            name.to_owned(),
            obj.get(field).cloned().unwrap_or(Value::Null),
        ))
    };
    read(RESULT_MIME, "value")
        .or_else(|| read(SCRAPBOOK_JSON_MIME, "data"))
        .or_else(|| read(SCRAPBOOK_TEXT_MIME, "data"))
}

/// The named results in some outputs, in order. When a name appears more
/// than once, the last output wins, as in scrapbook.
pub fn named_results_of<'a>(outputs: impl IntoIterator<Item = &'a Output>) -> Map<String, Value> {
    let mut results = Map::new();
    for output in outputs {
        let data = match output {
            Output::DisplayData(d) => &d.data,
            Output::ExecuteResult(r) => &r.data,
            _ => continue,
        };
        if let Some((name, value)) = named_result(data) {
            results.insert(name, value);
        }
    }
    results
}

/// The named results in a notebook's saved outputs, cell by cell.
pub fn named_results(nb: &Notebook) -> Map<String, Value> {
    named_results_of(nb.cells.iter().flat_map(|c| match c {
        Cell::Code(code) => code.outputs.as_slice(),
        _ => &[],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nb(cells: Value) -> Notebook {
        serde_json::from_value(json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": cells,
        }))
        .unwrap()
    }

    fn code(outputs: Value) -> Value {
        json!({ "cell_type": "code", "id": "c", "metadata": {}, "source": "", "execution_count": 1, "outputs": outputs })
    }

    #[test]
    fn reads_ganymede_and_scrapbook_results_last_wins() {
        let nb = nb(json!([
            code(json!([
                { "output_type": "display_data", "metadata": {}, "data": {
                    RESULT_MIME: { "name": "drop", "value": -0.19 }, "text/plain": "drop = -0.19" } },
                { "output_type": "stream", "name": "stdout", "text": "hi\n" },
                { "output_type": "display_data", "metadata": { "scrapbook": { "name": "rows", "data": true, "display": false } },
                  "data": { SCRAPBOOK_JSON_MIME: { "name": "rows", "data": [1, 2], "encoder": "json", "version": 1 } } },
            ])),
            { "cell_type": "markdown", "id": "m", "metadata": {}, "source": "x" },
            code(json!([
                { "output_type": "execute_result", "execution_count": 2, "metadata": {}, "data": {
                    SCRAPBOOK_TEXT_MIME: { "name": "label", "data": "west", "encoder": "text", "version": 1 } } },
                { "output_type": "display_data", "metadata": {}, "data": {
                    RESULT_MIME: { "name": "drop", "value": { "pct": -19 } } } },
            ])),
        ]));
        assert_eq!(
            Value::Object(named_results(&nb)),
            json!({ "drop": { "pct": -19 }, "rows": [1, 2], "label": "west" })
        );
    }

    #[test]
    fn ignores_malformed_results() {
        let nb = nb(json!([code(json!([
            { "output_type": "display_data", "metadata": {}, "data": { RESULT_MIME: { "value": 1 } } },
            { "output_type": "display_data", "metadata": {}, "data": { RESULT_MIME: { "name": "", "value": 1 } } },
            { "output_type": "display_data", "metadata": {}, "data": { RESULT_MIME: "not an object" } },
            { "output_type": "display_data", "metadata": {}, "data": { "application/json": { "name": "x", "value": 1 } } },
        ]))]));
        assert!(named_results(&nb).is_empty());
    }

    #[test]
    fn ganymede_type_wins_in_one_bundle_and_missing_value_is_null() {
        let mut bundle = MimeBundle::new();
        bundle.insert(SCRAPBOOK_JSON_MIME, json!({ "name": "a", "data": 1 }));
        bundle.insert(RESULT_MIME, json!({ "name": "b" }));
        assert_eq!(named_result(&bundle), Some(("b".into(), Value::Null)));
    }
}
