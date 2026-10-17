//! Fidelity tests against fixtures written by Python nbformat.
//!
//! Regenerate the fixtures with `tests/fixtures/generate.py`.

use std::path::{Path, PathBuf};

use jcore_nbformat::{Cell, MimeBundle, Notebook, Output};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Read a fixture written by `nbformat.write`, write it back, and compare bytes.
fn assert_roundtrip_identical(name: &str) {
    let original = read_fixture(name);
    let nb = Notebook::from_str(&original).unwrap_or_else(|e| panic!("{name}: {e}"));
    let written = nb.to_file_string();
    assert!(written == original, "{name}: Rust output differs from nbformat output.\n--- nbformat\n{original}\n--- rust\n{written}");
    assert_eq!(written.as_bytes(), original.as_bytes());

    // Reading what we wrote gives the same model.
    let again = Notebook::from_str(&written).unwrap();
    assert_eq!(again, nb);
}

#[test]
fn code_outputs_roundtrip_is_byte_identical() {
    assert_roundtrip_identical("code_outputs.ipynb");
}

#[test]
fn markdown_attachments_roundtrip_is_byte_identical() {
    assert_roundtrip_identical("markdown_attachments.ipynb");
}

#[test]
fn empty_roundtrip_is_byte_identical() {
    assert_roundtrip_identical("empty.ipynb");
}

#[test]
fn nbformat_expected_output_roundtrips() {
    assert_roundtrip_identical("unsplit_source.expected.ipynb");
}

#[test]
fn unsplit_input_is_written_as_nbformat_would() {
    let nb = Notebook::from_str(&read_fixture("unsplit_source.ipynb")).unwrap();
    let expected = read_fixture("unsplit_source.expected.ipynb");
    let written = nb.to_file_string();
    assert!(
        written == expected,
        "--- nbformat\n{expected}\n--- rust\n{written}"
    );

    // Transient metadata was stripped on read.
    assert!(!nb.metadata.contains_key("orig_nbformat"));
    assert!(!nb.metadata.contains_key("signature"));
    assert!(!nb.cells[0].metadata().contains_key("trusted"));
    assert_eq!(
        nb.cells[0].source().as_str(),
        "# Title\n\nParagraph one.\nParagraph two.\n"
    );
    assert_eq!(nb.cells[2].source().as_str(), "raw\r\ntext\r");
}

#[test]
fn code_outputs_model_is_what_the_generator_built() {
    let nb = Notebook::read_file(fixture("code_outputs.ipynb")).unwrap();
    assert_eq!(nb.nbformat, 4);
    assert_eq!(nb.nbformat_minor, 5);
    assert_eq!(
        nb.extra.get("x_extra_notebook_field"),
        Some(&json!({"nested": {"z": 1, "a": 2}}))
    );
    assert_eq!(nb.metadata["kernelspec"]["name"], "python3");

    let by_id = |id: &str| nb.cells.iter().find(|c| c.id() == Some(id)).unwrap();

    let Cell::Code(progress) = by_id("progress") else {
        panic!()
    };
    assert_eq!(progress.execution_count, Some(2));
    let Output::Stream(stdout) = &progress.outputs[0] else {
        panic!()
    };
    assert_eq!(stdout.name, "stdout");
    assert_eq!(
        stdout.text.as_str(),
        "\rprogress: 0%\rprogress: 50%\rprogress: 100%\n"
    );

    let Cell::Code(result) = by_id("result") else {
        panic!()
    };
    let Output::ExecuteResult(er) = &result.outputs[0] else {
        panic!()
    };
    assert_eq!(er.execution_count, Some(3));
    assert_eq!(
        er.data.get_str("text/plain"),
        Some("   a  b\n0  1  2\n1  3  4")
    );
    assert!(er
        .data
        .get_str("text/html")
        .unwrap()
        .starts_with("<div>\n<table"));
    assert_eq!(er.data.get("application/json").unwrap()["rows"][1]["b"], 4);
    assert_eq!(
        er.data.get_str("application/vnd.custom"),
        Some("line one\nline two\n")
    );
    assert_eq!(er.metadata["application/json"]["expanded"], true);

    let Cell::Code(display) = by_id("display") else {
        panic!()
    };
    let Output::DisplayData(png) = &display.outputs[0] else {
        panic!()
    };
    assert!(png
        .data
        .get_str("image/png")
        .unwrap()
        .starts_with("iVBORw0KGgo"));
    assert_eq!(png.metadata["image/png"]["width"], 1);
    let Output::DisplayData(svg) = &display.outputs[1] else {
        panic!()
    };
    assert!(svg
        .data
        .get_str("image/svg+xml")
        .unwrap()
        .ends_with("</svg>\n"));

    let Cell::Code(error) = by_id("error") else {
        panic!()
    };
    let Output::Error(err) = &error.outputs[0] else {
        panic!()
    };
    assert_eq!(err.ename, "ZeroDivisionError");
    assert_eq!(err.evalue, "division by zero");
    assert_eq!(err.traceback.len(), 4);
    assert!(err.traceback[0].starts_with("\u{1b}[0;31m"));
    // Traceback entries containing newlines are not split.
    assert!(err.traceback[2].contains('\n'));

    let Cell::Code(exotic) = by_id("exotic-line-breaks") else {
        panic!()
    };
    assert_eq!(exotic.execution_count, None);
    assert_eq!(
        exotic.source.as_str(),
        "a\nb\rc\r\nd\x0be\x0cf\x1cg\x1dh\x1ei\u{85}j\u{2028}k\u{2029}l\n\nno trailing newline"
    );

    let Cell::Code(empty) = by_id("empty-source") else {
        panic!()
    };
    assert_eq!(empty.source.as_str(), "");

    let Cell::Code(unknown) = by_id("unknown-fields") else {
        panic!()
    };
    assert_eq!(
        unknown.extra.get("x_extra_cell_field"),
        Some(&json!("kept"))
    );
    let Output::Stream(s) = &unknown.outputs[0] else {
        panic!()
    };
    assert_eq!(
        s.extra.get("x_extra_output_field"),
        Some(&json!({"k": [1, "two", null]}))
    );
}

#[test]
fn markdown_attachments_model() {
    let nb = Notebook::read_file(fixture("markdown_attachments.ipynb")).unwrap();
    let Cell::Markdown(intro) = &nb.cells[0] else {
        panic!()
    };
    assert!(intro
        .source
        .as_str()
        .starts_with("# Résumé — 日本語のノート 🚀\n"));
    assert!(intro.source.as_str().contains("👩‍🔬"));
    assert_eq!(intro.attachments, None);

    let Cell::Markdown(attached) = &nb.cells[1] else {
        panic!()
    };
    let attachments = attached.attachments.as_ref().unwrap();
    assert_eq!(
        attachments["notes.txt"].get_str("text/plain"),
        Some("first line\nsecond line\n")
    );
    assert_eq!(
        attachments["snippet.js"].get_str("application/javascript"),
        Some("let x = 1;\nlet y = 2;")
    );
    assert_eq!(
        attachments["data.json"].get("application/json"),
        Some(&json!({"a": [1, 2, 3], "s": "x\ny"}))
    );
    assert_eq!(attached.metadata["tags"], json!(["ünïcode"]));

    let Cell::Raw(raw) = &nb.cells[2] else {
        panic!()
    };
    assert_eq!(raw.metadata["format"], "text/restructuredtext");
    assert_eq!(
        raw.attachments.as_ref().unwrap()["raw.txt"].get_str("text/plain"),
        Some("raw cells may have attachments too")
    );
}

#[test]
fn write_file_and_read_file_through_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.ipynb");

    let mut nb = Notebook::new();
    nb.metadata.insert("zeta".into(), json!(1));
    nb.metadata
        .insert("alpha".into(), json!({"y": [1, 2], "x": "é"}));
    let mut data = MimeBundle::new();
    data.insert("text/plain", "1\n2");
    data.insert("application/json", json!({"b": 1, "a": 2}));
    nb.cells.push(Cell::Code(jcore_nbformat::CodeCell {
        id: Some("c1".into()),
        source: "print(1)\nprint(2)\n".into(),
        execution_count: Some(1),
        outputs: vec![
            Output::Stream(jcore_nbformat::StreamOutput {
                name: "stdout".into(),
                text: "1\n2\n".into(),
                ..Default::default()
            }),
            Output::ExecuteResult(jcore_nbformat::ExecuteResultOutput {
                execution_count: Some(1),
                data,
                ..Default::default()
            }),
        ],
        ..Default::default()
    }));
    nb.cells.push(Cell::Markdown(jcore_nbformat::MarkdownCell {
        id: Some("m1".into()),
        source: "# Hi".into(),
        ..Default::default()
    }));

    jcore_nbformat::write_file(&nb, &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"));
    assert_eq!(
        text,
        concat!(
            "{\n",
            " \"cells\": [\n",
            "  {\n",
            "   \"cell_type\": \"code\",\n",
            "   \"execution_count\": 1,\n",
            "   \"id\": \"c1\",\n",
            "   \"metadata\": {},\n",
            "   \"outputs\": [\n",
            "    {\n",
            "     \"name\": \"stdout\",\n",
            "     \"output_type\": \"stream\",\n",
            "     \"text\": [\n",
            "      \"1\\n\",\n",
            "      \"2\\n\"\n",
            "     ]\n",
            "    },\n",
            "    {\n",
            "     \"data\": {\n",
            "      \"application/json\": {\n",
            "       \"a\": 2,\n",
            "       \"b\": 1\n",
            "      },\n",
            "      \"text/plain\": [\n",
            "       \"1\\n\",\n",
            "       \"2\"\n",
            "      ]\n",
            "     },\n",
            "     \"execution_count\": 1,\n",
            "     \"metadata\": {},\n",
            "     \"output_type\": \"execute_result\"\n",
            "    }\n",
            "   ],\n",
            "   \"source\": [\n",
            "    \"print(1)\\n\",\n",
            "    \"print(2)\\n\"\n",
            "   ]\n",
            "  },\n",
            "  {\n",
            "   \"cell_type\": \"markdown\",\n",
            "   \"id\": \"m1\",\n",
            "   \"metadata\": {},\n",
            "   \"source\": [\n",
            "    \"# Hi\"\n",
            "   ]\n",
            "  }\n",
            " ],\n",
            " \"metadata\": {\n",
            "  \"alpha\": {\n",
            "   \"x\": \"é\",\n",
            "   \"y\": [\n",
            "    1,\n",
            "    2\n",
            "   ]\n",
            "  },\n",
            "  \"zeta\": 1\n",
            " },\n",
            " \"nbformat\": 4,\n",
            " \"nbformat_minor\": 5\n",
            "}\n",
        )
    );

    let back = jcore_nbformat::read_file(&path).unwrap();
    assert_eq!(back, nb);
    let via_reader = Notebook::from_reader(std::fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(via_reader, nb);
}
