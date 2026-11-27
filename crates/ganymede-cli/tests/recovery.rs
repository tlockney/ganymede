//! `ganymede serve` keeps recovery copies under `GANYMEDE_DATA_DIR`, in
//! the part of `recovery` that belongs to its root (#945).

mod common;

use common::*;
use serde_json::{json, Value};
use sha2::Digest;
use std::io::BufRead;
use std::process::Stdio;

#[test]
fn serve_offers_the_copies_in_its_data_dir() {
    let [root, state, data] = std::array::from_fn(|_| tempfile::tempdir().unwrap());
    let canonical = root.path().canonicalize().unwrap();
    let key = sha2::Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let copies = data.path().join("recovery").join(hex::encode(key));
    std::fs::create_dir_all(&copies).unwrap();
    let id = "0000000000000001-aaaaaaaa";
    let nb = json!({"cells": [], "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    std::fs::write(copies.join(format!("{id}.ipynb")), nb.to_string()).unwrap();
    let meta = json!({"path": "a.ipynb", "at": "2026-11-25T10:00:00Z", "reason": "save_failed",
        "detail": "", "file": canonical.join("a.ipynb"), "notebook": format!("{id}.ipynb")});
    std::fs::write(copies.join(format!("{id}.json")), meta.to_string()).unwrap();

    let mut child = KillOnDrop(
        ganymede(&["serve", "--port", "0", "--token", "t"])
            .arg("--root-dir")
            .arg(root.path())
            .arg("--notary-dir")
            .arg(state.path().join("notary"))
            .env("GANYMEDE_DATA_DIR", data.path())
            .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().split('?').next().unwrap();
    let url = format!(
        "{}/api/collaboration/recovery?path=a.ipynb",
        base.trim_end_matches('/')
    );
    let answer: Value = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let r = reqwest::Client::new()
            .get(url)
            .header("Authorization", "token t")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    });
    assert_eq!(answer["copies"][0]["id"], id, "{answer}");
}
