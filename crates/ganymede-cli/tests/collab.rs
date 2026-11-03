//! Document rooms against a `jupyter-collaboration` client (#258):
//! `e2e/collab/ydoc_client.py` syncs with a room through `jupyter_ydoc` and
//! `pycrdt`, the libraries `jupyter-collaboration` is built on, edits the
//! notebook while another program changes it on disk, and checks both
//! sides converge and the saved file holds every change. Python is a test
//! oracle here, as elsewhere in the e2e suite.
//!
//! Skips (printing `skipping:` to stderr) unless a Python with
//! `jupyter_ydoc`, `pycrdt` and `websockets` (15 or later) is found: `python3`, else
//! `python`. The e2e job installs them. When `GANYMEDE_E2E_PYTHON` names
//! a Python, that one is used and must have them: the test fails if it
//! can't import them.

mod common;

use common::*;
use std::io::BufRead;
use std::process::{Command, Stdio};

const TOKEN: &str = "e2e-token";

/// The client's imports, and websockets 15 or later: `ydoc_client.py`
/// passes `proxy=None`, which older versions reject.
const MODULES: &str = "import jupyter_ydoc, pycrdt, websockets; \
    assert int(websockets.__version__.split('.')[0]) >= 15, \
    f'websockets {websockets.__version__} is older than 15'";

fn has_modules(python: &str) -> bool {
    Command::new(python)
        .args(["-c", MODULES])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The Python to run the client with. `GANYMEDE_E2E_PYTHON` must have the
/// modules; without it, a `python3` or `python` that has them, if any.
fn python() -> Option<String> {
    if let Ok(p) = std::env::var("GANYMEDE_E2E_PYTHON") {
        let out = Command::new(&p).args(["-c", MODULES]).output();
        match out {
            Ok(o) if o.status.success() => return Some(p),
            Ok(o) => panic!(
                "GANYMEDE_E2E_PYTHON={p} can't `{MODULES}`:\n{}",
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => panic!("GANYMEDE_E2E_PYTHON={p} doesn't run: {e}"),
        }
    }
    ["python3", "python"]
        .into_iter()
        .find(|p| has_modules(p))
        .map(str::to_string)
}

#[test]
fn jupyter_ydoc_client_converges_with_a_room_and_the_file() {
    let Some(py) = python() else {
        eprintln!("skipping: no Python with jupyter_ydoc, pycrdt and websockets >= 15");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let nb = serde_json::json!({
        "cells": [
            {"cell_type": "code", "execution_count": null, "id": "a", "metadata": {}, "outputs": [], "source": "x = 1"},
            {"cell_type": "code", "execution_count": null, "id": "b", "metadata": {}, "outputs": [], "source": "y = 2"}
        ],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5
    });
    std::fs::write(
        root.path().join("shared.ipynb"),
        serde_json::to_string_pretty(&nb).unwrap(),
    )
    .unwrap();

    let mut serve = KillOnDrop(
        ganymede(&["serve", "--port", "0", "--token", TOKEN])
            .arg("--root-dir")
            .arg(root.path())
            .arg("--config-dir")
            .arg(state.path())
            .arg("--notary-dir")
            .arg(state.path().join("notary"))
            .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .env("JUPYTER_DATA_DIR", state.path().join("data"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(serve.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().split('?').next().unwrap().trim_end_matches('/');

    let out = Command::new(&py)
        .arg(repo().join("e2e/collab/ydoc_client.py"))
        .arg(base)
        .arg(TOKEN)
        .arg(root.path())
        .arg("shared.ipynb")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "client failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The file is valid nbformat as Ganymede writes it.
    let saved = std::fs::read_to_string(root.path().join("shared.ipynb")).unwrap();
    let parsed = ganymede_nbformat::Notebook::from_str(&saved).unwrap();
    assert_eq!(parsed.to_file_string(), saved);
    assert_eq!(parsed.cells.len(), 3);
}
