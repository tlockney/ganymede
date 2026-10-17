//! `ganymede lab install` against PyPI, then `ganymede serve` with no
//! `--static-dir` serving the installed JupyterLab. It downloads about
//! 17 MB, so it runs only with `GANYMEDE_E2E_LAB=1` (CI's e2e job sets it).
//! Extraction itself is unit tested against a fixture wheel in `src/lab.rs`.

mod common;

use common::*;
use std::io::BufRead;
use std::process::Stdio;

#[test]
fn install_then_serve_jupyterlab() {
    if std::env::var("GANYMEDE_E2E_LAB").as_deref() != Ok("1") {
        eprintln!("skipping: set GANYMEDE_E2E_LAB=1 to download JupyterLab from PyPI");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let lab = |args: &[&str]| {
        let mut c = ganymede(args);
        c.env("GANYMEDE_DATA_DIR", data.path());
        c
    };

    let out = run_ok(lab(&["lab", "install"]));
    let dir = std::path::PathBuf::from(stdout(&out).trim());
    assert!(dir.starts_with(data.path()), "{}", dir.display());
    for f in ["static/index.html", "static/package.json", "LICENSE"] {
        assert!(dir.join(f).is_file(), "{f} missing in {}", dir.display());
    }
    assert!(dir.join("schemas/@jupyterlab").is_dir());
    assert!(dir.join("themes/@jupyterlab").is_dir());
    let version = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(stdout(&run_ok(lab(&["lab", "list"]))).starts_with(&version));

    let mut child = KillOnDrop(
        lab(&["serve", "--port", "0", "--token", "secret", "--root-dir"])
            .arg(root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().split('?').next().unwrap().to_string();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let http = reqwest::Client::new();
        let get = |p: &str| {
            http.get(format!("{base}{p}"))
                .header("Authorization", "token secret")
                .send()
        };
        let r = get("lab").await.unwrap();
        assert_eq!(r.status(), 200);
        let page = r.text().await.unwrap();
        assert!(page.contains("jupyter-config-data"), "{page}");
        assert!(page.contains(&format!("\"appVersion\":\"{version}\"")));
        let r = get("lab/api/settings").await.unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        let settings = v["settings"].as_array().unwrap();
        assert!(
            settings
                .iter()
                .any(|s| s["id"] == "@jupyterlab/apputils-extension:themes"),
            "{v}"
        );
    });
    drop(child);

    run_ok(lab(&["lab", "remove", &version]));
    assert!(!dir.exists());
}

/// Stops the server even when an assertion fails.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
