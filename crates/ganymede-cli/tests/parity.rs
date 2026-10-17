//! Notebooks executed by `ganymede execute` must match `jupyter execute`
//! (nbclient) byte for byte, once timestamps and random ids are
//! normalized. Every notebook under `e2e/notebooks/` runs on the kernel its
//! metadata names, through both, on the same kernelspec.
//!
//! Where streams are split differs run to run: a kernel flushes stdout on a
//! timer, so `0\n1\n2\n` can arrive as one chunk or two whichever client
//! reads it. When the bytes differ, the comparison falls back to the
//! notebooks with consecutive same-name streams merged, which is what a
//! reader sees.

mod common;

use common::*;
use std::path::Path;

fn compare(nb: &Path) {
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(nb).unwrap()).unwrap();
    let kernel = spec["metadata"]["kernelspec"]["name"].as_str().unwrap();
    if !have_kernel(kernel) {
        return;
    }
    let Some(jupyter) = jupyter() else { return };
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.ipynb");
    std::fs::copy(nb, &input).unwrap();

    let reference = dir.path().join("reference.ipynb");
    let out = std::process::Command::new(&jupyter)
        .args(["execute", "input.ipynb", "--output=reference.ipynb"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "jupyter execute failed on {}: {}",
        nb.display(),
        stderr(&out)
    );

    let ours = dir.path().join("ganymede.ipynb");
    run_ok(
        ganymede(&["execute", "-q"])
            .arg(&input)
            .arg("-o")
            .arg(&ours),
    );

    let a = normalize(&std::fs::read_to_string(&reference).unwrap());
    let b = normalize(&std::fs::read_to_string(&ours).unwrap());
    if a == b {
        return;
    }
    let (a, b) = (merge_streams(&a), merge_streams(&b));
    if a != b {
        let keep = std::env::temp_dir().join(format!(
            "ganymede-parity-{}",
            nb.file_stem().unwrap().to_string_lossy()
        ));
        let _ = std::fs::create_dir_all(&keep);
        let _ = std::fs::copy(&reference, keep.join("reference.ipynb"));
        let _ = std::fs::copy(&ours, keep.join("ganymede.ipynb"));
        panic!(
            "{} differs from jupyter execute ({} kept in {}):\n{}",
            nb.display(),
            kernel,
            keep.display(),
            first_difference(&a, &b)
        );
    }
}

/// The notebook with each cell's consecutive same-name stream outputs
/// joined, pretty-printed.
fn merge_streams(text: &str) -> String {
    let mut nb: serde_json::Value = serde_json::from_str(text).unwrap();
    for cell in nb["cells"].as_array_mut().unwrap() {
        let Some(outputs) = cell.get_mut("outputs").and_then(|o| o.as_array_mut()) else {
            continue;
        };
        let mut merged: Vec<serde_json::Value> = Vec::new();
        for o in outputs.drain(..) {
            let text = |v: &serde_json::Value| match &v["text"] {
                serde_json::Value::Array(a) => a.iter().filter_map(|s| s.as_str()).collect(),
                v => v.as_str().unwrap_or_default().to_string(),
            };
            if let Some(last) = merged.last_mut() {
                if o["output_type"] == "stream"
                    && last["output_type"] == "stream"
                    && o["name"] == last["name"]
                {
                    last["text"] = (text(last) + &text(&o)).into();
                    continue;
                }
            }
            let mut o = o;
            if o["output_type"] == "stream" {
                o["text"] = text(&o).into();
            }
            merged.push(o);
        }
        *outputs = merged;
    }
    serde_json::to_string_pretty(&nb).unwrap()
}

fn notebooks(lang: &str) -> Vec<std::path::PathBuf> {
    let dir = repo().join("e2e/notebooks").join(lang);
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ipynb"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no notebooks in {}", dir.display());
    v
}

#[test]
fn typescript_notebooks_match_jupyter_execute() {
    for nb in notebooks("typescript") {
        compare(&nb);
    }
}

#[test]
fn python_notebooks_match_jupyter_execute() {
    for nb in notebooks("python") {
        compare(&nb);
    }
}

#[test]
fn normalize_replaces_times_and_ids_consistently() {
    let s = r#"{"a":"2026-10-17T16:41:06.123456Z","m":"6d9730a0-dc27-45ac-a296-0c918e8689b4","r":"Widget(6d9730a0-dc27-45ac-a296-0c918e8689b4)","k":"0123456789abcdef0123456789abcdef","not":"6d9730a0x"}"#;
    assert_eq!(
        normalize(s),
        r#"{"a":"<timestamp>","m":"<id0>","r":"Widget(<id0>)","k":"<id1>","not":"6d9730a0x"}"#
    );
}
