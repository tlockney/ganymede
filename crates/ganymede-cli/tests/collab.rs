//! Document rooms against a `jupyter-collaboration` client (#258):
//! `e2e/collab/ydoc_client.py` syncs with a room through `jupyter_ydoc` and
//! `pycrdt`, the libraries `jupyter-collaboration` is built on, edits the
//! notebook while another program changes it on disk, and checks both
//! sides converge and the saved file holds every change. Python is a test
//! oracle here, as elsewhere in the e2e suite.
//!
//! Skips (printing `skipping:`) unless a Python with `jupyter_ydoc`,
//! `pycrdt` and `websockets` is found: `GANYMEDE_E2E_PYTHON`, else
//! `python3`, else `python`. The e2e job installs them.

mod common;

use common::*;
use std::io::BufRead;
use std::process::{Command, Stdio};

const TOKEN: &str = "e2e-token";

fn python() -> Option<String> {
    let candidates = match std::env::var("GANYMEDE_E2E_PYTHON") {
        Ok(p) => vec![p],
        Err(_) => vec!["python3".into(), "python".into()],
    };
    candidates.into_iter().find(|p| {
        Command::new(p)
            .args(["-c", "import jupyter_ydoc, pycrdt, websockets"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

struct Serve(std::process::Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn jupyter_ydoc_client_converges_with_a_room_and_the_file() {
    let Some(py) = python() else {
        println!("skipping: no Python with jupyter_ydoc, pycrdt and websockets");
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

    let mut child = ganymede(&["serve", "--port", "0", "--token", TOKEN])
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
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _serve = Serve(child);
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
