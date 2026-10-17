//! `ganymede lab` with no subcommand, offline: a stand-in JupyterLab
//! bundle installed under `GANYMEDE_DATA_DIR`, a taken port it must move
//! past, and the printed URL logging a browser in.

mod common;

use common::*;
use std::io::BufRead;
use std::path::Path;
use std::process::Stdio;

/// The least `Lab::detect` and `installed` accept: a templated
/// `index.html` and a `package.json` naming the app.
fn fake_bundle(data: &Path, version: &str) {
    let static_dir = data.join("lab").join(version).join("static");
    std::fs::create_dir_all(&static_dir).unwrap();
    std::fs::write(
        static_dir.join("index.html"),
        r#"<html><head><script id="jupyter-config-data" type="application/json">{{ page_config_full | tojson }}</script></head><body></body></html>"#,
    )
    .unwrap();
    std::fs::write(
        static_dir.join("package.json"),
        format!(r#"{{"name":"@jupyterlab/application-top","version":"{version}","jupyterlab":{{"name":"JupyterLab","version":"{version}"}}}}"#),
    )
    .unwrap();
}

#[test]
fn lab_serves_installed_bundle_and_logs_in() {
    let data = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    fake_bundle(data.path(), "4.6.4");

    // A port that is taken, so `lab` has to fall back to a later one.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();

    let mut child = KillOnDrop(
        ganymede(&["lab", "--no-browser", "--port", &port.to_string()])
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
    assert!(page.ends_with("/lab"), "{url}");
    assert!(!token.is_empty(), "{url}");
    let got: u16 = page
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/lab")
        .parse()
        .expect(&url);
    assert!(got > port, "{url} should be past the taken port {port}");

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        // Without the token, the page sends a browser to log in.
        let r = http.get(page).send().await.unwrap();
        assert_eq!(r.status(), 302);
        assert!(r.headers()["location"].to_str().unwrap().contains("login"));

        // The printed URL sets the login cookie and drops the token.
        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 302);
        assert_eq!(r.headers()["location"], "/lab");
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

        // The cookie alone opens JupyterLab.
        let r = http
            .get(page)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = r.text().await.unwrap();
        assert!(body.contains("\"appVersion\":\"4.6.4\""), "{body}");
    });
    drop(taken);
}

#[test]
fn lab_without_bundle_says_how_to_install() {
    let data = tempfile::tempdir().unwrap();
    let out = ganymede(&["lab", "--no-browser", "--port", "0"])
        .env("GANYMEDE_DATA_DIR", data.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("ganymede lab install"),
        "{}",
        stderr(&out)
    );
}

/// Stops the server even when an assertion fails.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
