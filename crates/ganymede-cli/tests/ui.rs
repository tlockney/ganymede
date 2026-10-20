//! `ganymede ui`: offline, a stand-in ganymede-ui install under
//! `GANYMEDE_DATA_DIR` served at `/` behind the printed URL's login; and,
//! with `GANYMEDE_E2E_UI=1` (CI's e2e job sets it), `ganymede ui install`
//! downloading the real release from GitHub.

mod common;

use common::*;
use std::io::BufRead;
use std::path::Path;
use std::process::Stdio;

fn fake_install(data: &Path, version: &str) {
    let dir = data.join("ui").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        format!("<!doctype html><title>ganymede-ui {version}</title>"),
    )
    .unwrap();
}

#[test]
fn ui_serves_the_newest_install_and_logs_in() {
    let data = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fake_install(data.path(), "0.1.0");
    fake_install(data.path(), "0.2.0");

    let out = run_ok(ganymede(&["ui", "list"]).env("GANYMEDE_DATA_DIR", data.path()));
    let versions: Vec<_> = stdout(&out)
        .lines()
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(versions, ["0.2.0", "0.1.0"]);

    let mut child = KillOnDrop(
        ganymede(&["ui", "--no-browser", "--port", "0"])
            .current_dir(root.path())
            .env("GANYMEDE_DATA_DIR", data.path())
            .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
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
    let url = line.trim().to_string();
    let (page, token) = url.split_once("?token=").expect(&url);
    assert!(page.ends_with('/'), "{url}");
    assert!(!token.is_empty(), "{url}");

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 302);
        let cookie = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .find(|c| c.starts_with("username-"))
            .expect("login cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string();

        // The newest install, with the `_xsrf` cookie the app needs.
        let r = http
            .get(page)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(r
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|c| c.to_str().unwrap().starts_with("_xsrf=")));
        let body = r.text().await.unwrap();
        assert!(body.contains("ganymede-ui 0.2.0"), "{body}");
    });
}

#[test]
fn ui_without_install_says_how_to_install() {
    let data = tempfile::tempdir().unwrap();
    let out = ganymede(&["ui", "--no-browser", "--port", "0"])
        .env("GANYMEDE_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("ganymede ui install"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn install_from_github() {
    if std::env::var("GANYMEDE_E2E_UI").as_deref() != Ok("1") {
        eprintln!("skipping: set GANYMEDE_E2E_UI=1 to download ganymede-ui from GitHub");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let ui = |args: &[&str]| {
        let mut c = ganymede(args);
        c.env("GANYMEDE_DATA_DIR", data.path());
        c
    };
    let out = run_ok(ui(&["ui", "install"]));
    let dir = std::path::PathBuf::from(stdout(&out).trim());
    assert!(dir.starts_with(data.path()), "{}", dir.display());
    for f in ["index.html", "sw.js", "manifest.webmanifest", "LICENSE"] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    // Again: already installed, nothing downloaded.
    let out = run_ok(ui(&["ui", "install"]));
    assert!(
        stderr(&out).contains("already installed"),
        "{}",
        stderr(&out)
    );
    // The default version, whatever it is: the install directory's name.
    let version = dir.file_name().unwrap().to_str().unwrap().to_owned();
    run_ok(ui(&["ui", "remove", &version]));
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
