//! Notebook trust: signatures against fixtures computed by Python nbformat
//! (`tests/fixtures/generate_trust.py`), and `FsContents` marking, signing
//! and trusting notebooks as `jupyter_server` does.

use std::path::Path;
use std::sync::Arc;

use ganymede_contents::trust::{self, MemorySignatureStore, Notary};
use ganymede_contents::{ContentType, Contents, FsContents, FsOptions, GetOptions, SaveRequest};
use ganymede_nbformat::Notebook;
use serde_json::{json, Value};

fn fixtures() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trust.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn notary_with(secret: &str) -> Notary {
    Notary::new(secret.as_bytes(), Box::new(MemorySignatureStore::new()))
}

#[test]
fn signatures_match_nbformat() {
    let f = fixtures();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cases = f["notebooks"].as_array().unwrap();
    assert!(cases.len() >= 4);
    for case in cases {
        let name = case["notebook"].as_str().unwrap();
        // What jupyter_server hashes: the notebook as nbformat.read returns
        // it, which is what Ganymede serves.
        let nb = Notebook::read_file(dir.join(name)).unwrap();
        let nb = serde_json::to_value(&nb).unwrap();
        for (label, expected) in case["signatures"].as_object().unwrap() {
            let secret = f["secrets"][label].as_str().unwrap();
            assert_eq!(
                notary_with(secret).compute_signature(&nb),
                expected.as_str().unwrap(),
                "{name} with the {label} secret"
            );
        }
    }
}

#[test]
fn signature_of_a_raw_dict_matches_nbformat() {
    let f = fixtures();
    let raw = &f["raw"];
    let secret = f["secrets"][raw["secret"].as_str().unwrap()]
        .as_str()
        .unwrap();
    assert_eq!(
        notary_with(secret).compute_signature(&raw["notebook"]),
        raw["signature"].as_str().unwrap()
    );
}

#[test]
fn check_cells_matches_nbformat() {
    for case in fixtures()["check_cells"].as_array().unwrap() {
        let mut nb = case["notebook"].clone();
        let label = case["case"].as_str().unwrap();
        assert_eq!(
            trust::check_cells(&mut nb),
            case["trusted"].as_bool().unwrap(),
            "{label}"
        );
        assert_eq!(nb, case["after"], "{label}: trusted flags left behind");
    }
}

fn setup(notary: Option<Arc<Notary>>) -> (tempfile::TempDir, FsContents) {
    let dir = tempfile::tempdir().unwrap();
    let contents = FsContents::new(
        dir.path(),
        FsOptions {
            notary,
            ..FsOptions::default()
        },
    )
    .unwrap();
    (dir, contents)
}

fn memory_notary() -> Arc<Notary> {
    Arc::new(notary_with("test"))
}

fn html_notebook(trusted: Option<bool>) -> Value {
    let mut metadata = json!({});
    if let Some(t) = trusted {
        metadata["trusted"] = json!(t);
    }
    json!({
        "nbformat": 4, "nbformat_minor": 5, "metadata": {},
        "cells": [
            {"cell_type": "markdown", "id": "m", "metadata": {}, "source": "# hi"},
            {"cell_type": "code", "id": "c", "metadata": metadata, "source": "html()", "execution_count": 1,
             "outputs": [{"output_type": "display_data", "metadata": {},
                          "data": {"text/html": "<b>bold</b>\n<i>x</i>", "text/plain": "html"}}]}
        ]
    })
}

async fn trusted_flags(contents: &FsContents, path: &str) -> Vec<Option<bool>> {
    let model = contents
        .get(path, GetOptions::with_content())
        .await
        .unwrap();
    model.content.unwrap()["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["metadata"].get("trusted").and_then(Value::as_bool))
        .collect()
}

#[tokio::test]
async fn rich_output_from_this_session_is_signed_on_save() {
    let (_dir, contents) = setup(Some(memory_notary()));
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(Some(true))))
        .await
        .unwrap();
    // Only code cells are marked, as nbformat's mark_cells does.
    assert_eq!(
        trusted_flags(&contents, "a.ipynb").await,
        [None, Some(true)]
    );
    // The flag is not written to disk.
    let on_disk = std::fs::read_to_string(_dir.path().join("a.ipynb")).unwrap();
    assert!(!on_disk.contains("trusted"));
}

#[tokio::test]
async fn untrusted_rich_output_is_not_signed() {
    let (_dir, contents) = setup(Some(memory_notary()));
    for flag in [None, Some(false)] {
        contents
            .save("a.ipynb", SaveRequest::notebook(html_notebook(flag)))
            .await
            .unwrap();
        assert_eq!(
            trusted_flags(&contents, "a.ipynb").await,
            [None, Some(false)]
        );
    }
}

#[tokio::test]
async fn safe_outputs_are_signed_without_a_flag() {
    let (_dir, contents) = setup(Some(memory_notary()));
    let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
        {"cell_type": "code", "id": "c", "metadata": {}, "source": "print(1)", "execution_count": 1,
         "outputs": [{"output_type": "stream", "name": "stdout", "text": "1\n"}]}
    ]});
    contents
        .save("a.ipynb", SaveRequest::notebook(nb))
        .await
        .unwrap();
    assert_eq!(trusted_flags(&contents, "a.ipynb").await, [Some(true)]);
}

#[tokio::test]
async fn editing_on_disk_drops_trust() {
    let (dir, contents) = setup(Some(memory_notary()));
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(Some(true))))
        .await
        .unwrap();
    let path = dir.path().join("a.ipynb");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("<b>bold</b>", "<script>x</script>")).unwrap();
    assert_eq!(
        trusted_flags(&contents, "a.ipynb").await,
        [None, Some(false)]
    );
}

#[tokio::test]
async fn trust_signs_whatever_the_outputs() {
    let (_dir, contents) = setup(Some(memory_notary()));
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(None)))
        .await
        .unwrap();
    assert_eq!(
        trusted_flags(&contents, "a.ipynb").await,
        [None, Some(false)]
    );
    contents.trust("a.ipynb").await.unwrap();
    assert_eq!(
        trusted_flags(&contents, "a.ipynb").await,
        [None, Some(true)]
    );

    // Saving back what was served keeps it trusted: the client returns the
    // cells' `trusted: true`.
    let served = contents
        .get("a.ipynb", GetOptions::with_content())
        .await
        .unwrap()
        .content
        .unwrap();
    contents
        .save("b.ipynb", SaveRequest::notebook(served))
        .await
        .unwrap();
    assert_eq!(
        trusted_flags(&contents, "b.ipynb").await,
        [None, Some(true)]
    );

    contents
        .save("t.txt", SaveRequest::text("x"))
        .await
        .unwrap();
    assert_eq!(contents.trust("t.txt").await.unwrap_err().status(), 400);
    assert_eq!(
        contents.trust("missing.ipynb").await.unwrap_err().status(),
        404
    );
}

#[tokio::test]
async fn copies_and_new_notebooks_are_trusted() {
    let (_dir, contents) = setup(Some(memory_notary()));
    let model = contents
        .new_untitled("", ContentType::Notebook, None)
        .await
        .unwrap();
    assert_eq!(model.name, "Untitled.ipynb");
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(Some(true))))
        .await
        .unwrap();
    let copy = contents.copy("a.ipynb", None).await.unwrap();
    assert_eq!(
        trusted_flags(&contents, &copy.path).await,
        [None, Some(true)]
    );
}

#[tokio::test]
async fn without_a_notary_nothing_is_marked() {
    let (_dir, contents) = setup(None);
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(Some(true))))
        .await
        .unwrap();
    assert_eq!(trusted_flags(&contents, "a.ipynb").await, [None, None]);
    assert_eq!(contents.trust("a.ipynb").await.unwrap_err().status(), 400);
}

#[tokio::test]
async fn trust_survives_a_restart_with_the_same_data_dir() {
    let data = tempfile::tempdir().unwrap();
    let (root, contents) = setup(Some(Arc::new(Notary::open(data.path()).unwrap())));
    contents
        .save("a.ipynb", SaveRequest::notebook(html_notebook(Some(true))))
        .await
        .unwrap();
    let again = FsContents::new(
        root.path(),
        FsOptions {
            notary: Some(Arc::new(Notary::open(data.path()).unwrap())),
            ..FsOptions::default()
        },
    )
    .unwrap();
    let expected = if cfg!(feature = "sqlite") {
        Some(true)
    } else {
        // Without SQLite, signatures last as long as the process's notary.
        Some(false)
    };
    assert_eq!(trusted_flags(&again, "a.ipynb").await, [None, expected]);
}
