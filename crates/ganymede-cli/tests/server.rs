//! The server against the reference: one scenario, driven over HTTP and
//! the websocket exactly as a frontend would, against `ganymede serve` and
//! against `jupyter server` (jupyter_server) on the same kernelspec. Every
//! step records what a client can observe (status codes, model keys and
//! values, the iopub messages each request produced, reply contents); the
//! two transcripts must be equal once timestamps and random ids are
//! normalized. That includes logging in the way a browser does: the login
//! cookie a token sets, `/login` and `/logout`, the `_xsrf` check, and a
//! websocket authenticated by the cookie alone.
//!
//! A second test crashes the kernel mid-cell on both servers and checks
//! what they share (`status: restarting`, the same kernel id, state gone)
//! rather than a transcript, since Ganymede also answers the crashed cell.

mod common;

use common::*;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const TOKEN: &str = "e2e-token";

struct Code {
    kernel: &'static str,
    /// jupyter_client swaps in ipykernel's own spec for `python3` (argv
    /// starting `python`, plus `kernel_protocol_version`) instead of the
    /// installed kernel.json. Ganymede serves the file; these fields are
    /// left out of the comparison for that kernel.
    native_spec_substituted: bool,
    set_state: &'static str,
    answer: &'static str,
    stream: &'static str,
    display: &'static str,
    raise: &'static str,
    input: &'static str,
    sleep: &'static str,
    read_state: &'static str,
}

const CODE: &[Code] = &[
    Code {
        kernel: "cdp-node",
        native_spec_substituted: false,
        set_state: "var kept = 'state'",
        answer: "6 * 7",
        stream: "console.log('out')",
        display: "await jupyter.display(jupyter.md`**md**`)",
        raise: "throw new TypeError('bad')",
        input: "console.log('hi ' + prompt('name? '))",
        sleep: "await new Promise((r) => setTimeout(r, 30000))",
        read_state: "kept",
    },
    Code {
        kernel: "python3",
        native_spec_substituted: true,
        set_state: "kept = 'state'",
        answer: "6 * 7",
        stream: "print('out')",
        display: "from IPython.display import Markdown, display; display(Markdown('**md**'))",
        raise: "raise TypeError('bad')",
        input: "print('hi ' + input('name? '))",
        sleep: "import time; time.sleep(30)",
        read_state: "kept",
    },
];

struct Server {
    child: Child,
    base: String,
    _root: tempfile::TempDir,
    _state: tempfile::TempDir,
}

impl Drop for Server {
    /// SIGTERM, then SIGKILL after 10 s: a server stuck shutting down
    /// (jupyter_server with a kernel mid-restart) must not hang the test.
    /// Its kernels run in their own process groups, so a killed server can
    /// leave them behind.
    fn drop(&mut self) {
        let pid = self.child.id() as i32;
        unsafe {
            kill(pid, 15);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("server {pid} still running 10 s after SIGTERM; killing it");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_ganymede() -> Server {
    start_ganymede_with(&[])
}

fn start_ganymede_with(extra: &[&std::ffi::OsStr]) -> Server {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut child = ganymede(&["serve", "--port", "0", "--token", TOKEN])
        .args(extra)
        .arg("--root-dir")
        .arg(root.path())
        .arg("--config-dir")
        .arg(state.path())
        .arg("--notary-dir")
        .arg(state.path().join("notary"))
        .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().split('?').next().unwrap().to_string();
    Server {
        child,
        base,
        _root: root,
        _state: state,
    }
}

fn start_jupyter(jupyter: &std::path::Path) -> Server {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let port = free_port();
    // nbformat writes the signing key there but does not create the dir.
    std::fs::create_dir(state.path().join("notary")).unwrap();
    let child = Command::new(jupyter)
        .args([
            "server",
            "--no-browser",
            "--allow-root",
            &format!("--ServerApp.port={port}"),
            "--ServerApp.port_retries=0",
            &format!("--IdentityProvider.token={TOKEN}"),
            // Delete for good, as ganymede does. On a fresh home, two
            // servers trashing at once race to create ~/.local/share/Trash
            // (send2trash checks, then makedirs) and one answers 400.
            "--FileContentsManager.delete_to_trash=False",
        ])
        .arg(format!("--ServerApp.root_dir={}", root.path().display()))
        .arg(format!(
            "--NotebookNotary.data_dir={}",
            state.path().join("notary").display()
        ))
        .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
        .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Server {
        child,
        base: format!("http://127.0.0.1:{port}/"),
        _root: root,
        _state: state,
    }
}

struct Client {
    http: reqwest::Client,
    base: String,
    transcript: Vec<(String, Value)>,
}

impl Client {
    fn record(&mut self, step: &str, v: Value) {
        self.transcript.push((step.to_string(), v));
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Authorization", format!("token {TOKEN}"));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A request with only the given headers: no token unless they carry
    /// one, and no redirects followed.
    async fn raw(
        &self,
        method: reqwest::Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, String)>,
    ) -> (u16, reqwest::header::HeaderMap, String) {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        if let Some((content_type, b)) = body {
            r = r.header("Content-Type", content_type).body(b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        (status, headers, resp.text().await.unwrap())
    }

    /// `username-<host>`, the login cookie's name for this server.
    fn login_name(&self) -> String {
        let host = self.base.split("://").nth(1).unwrap().trim_end_matches('/');
        format!("username-{host}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }

    /// What a browser learns from each `Set-Cookie`: the name, whether the
    /// value is empty (a cleared cookie), and the attributes. Expiry dates
    /// differ run to run, and Ganymede adds `SameSite=Lax`, which
    /// jupyter_server does not send; both are left out.
    fn set_cookies(&self, h: &reqwest::header::HeaderMap) -> Value {
        let login = self.login_name();
        let mut out: Vec<Value> = h
            .get_all("set-cookie")
            .iter()
            .map(|v| {
                let mut parts = v.to_str().unwrap().split(';').map(str::trim);
                let (name, value) = parts.next().unwrap().split_once('=').unwrap();
                let mut attrs: Vec<String> = parts
                    .map(|a| a.to_ascii_lowercase())
                    .filter(|a| !a.starts_with("expires=") && !a.starts_with("samesite="))
                    .collect();
                attrs.sort();
                json!({
                    "name": if name == login { "<login>" } else { name },
                    "cleared": value.trim_matches('"').is_empty(),
                    "attrs": attrs,
                })
            })
            .collect();
        out.sort_by_key(|v| v["name"].to_string());
        json!(out)
    }

    async fn wait_ready(&self) {
        for _ in 0..200 {
            if let Ok(r) = self.http.get(format!("{}api", self.base)).send().await {
                if r.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("server at {} did not come up", self.base);
    }
}

fn keys(v: &Value) -> Value {
    let mut k: Vec<&String> = v
        .as_object()
        .map(|o| o.keys().collect())
        .unwrap_or_default();
    k.sort();
    json!(k)
}

fn pick(v: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for f in fields {
        out.insert(f.to_string(), v.get(*f).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn request(msg_type: &str, channel: &str, content: Value) -> Value {
    json!({
        "header": {
            "msg_id": uuid(),
            "msg_type": msg_type,
            "session": "e2e-session",
            "username": "e2e",
            "date": "2026-01-01T00:00:00.000000Z",
            "version": "5.3",
        },
        "parent_header": {},
        "metadata": {},
        "content": content,
        "channel": channel,
        "buffers": [],
    })
}

fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!(
        "{:032x}",
        N.fetch_add(1, Ordering::Relaxed) + 0xe2e0_0000_0000
    )
}

/// Send `msg` and collect, until its reply and idle have both arrived,
/// what a frontend sees for it: each iopub message's type and content,
/// stdin requests (answered with `answer`), and the reply.
async fn exchange(ws: &mut Ws, msg: Value, answer: Option<&str>) -> Value {
    exchange_step("exchange", ws, msg, answer).await
}

/// [`exchange`], failing with `step` in the message if 60 s pass with no
/// frame.
async fn exchange_step(step: &str, ws: &mut Ws, msg: Value, answer: Option<&str>) -> Value {
    let id = msg["header"]["msg_id"].as_str().unwrap().to_string();
    ws.send(WsMessage::text(msg.to_string())).await.unwrap();
    let mut iopub = Vec::new();
    let mut stdin = Vec::new();
    let mut reply = None;
    let mut idle = false;
    while reply.is_none() || !idle {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .unwrap_or_else(|_| panic!("{step}: no message in 60 s"))
            .unwrap()
            .unwrap();
        let WsMessage::Text(text) = frame else {
            continue;
        };
        let m: Value = serde_json::from_str(&text).unwrap();
        if m["parent_header"]["msg_id"] != id.as_str() {
            continue;
        }
        let msg_type = m["header"]["msg_type"].as_str().unwrap().to_string();
        match m["channel"].as_str().unwrap() {
            "iopub" => {
                if msg_type == "status" && m["content"]["execution_state"] == "idle" {
                    idle = true;
                }
                iopub.push(json!([msg_type, m["content"]]));
            }
            "stdin" => {
                stdin.push(json!([msg_type, m["content"]]));
                let mut r = request(
                    "input_reply",
                    "stdin",
                    json!({"value": answer.unwrap_or_default()}),
                );
                r["parent_header"] = m["header"].clone();
                ws.send(WsMessage::text(r.to_string())).await.unwrap();
            }
            ch => {
                reply = Some(json!({"channel": ch, "msg_type": msg_type, "content": m["content"]}));
            }
        }
    }
    json!({"iopub": iopub, "stdin": stdin, "reply": reply})
}

/// `name=value` of the `Set-Cookie` called `name`, to send back.
fn cookie(h: &reqwest::header::HeaderMap, name: &str) -> String {
    h.get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .find(|nv| nv.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} cookie in {h:?}"))
}

fn message(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap_or(Value::Null)["message"].clone()
}

/// Logging in as a browser does. Returns the login and `_xsrf` cookies.
async fn browser_login(cl: &mut Client) -> String {
    use reqwest::Method as M;
    let (s, _, body) = cl.raw(M::GET, "api/status", &[], None).await;
    cl.record(
        "no credentials",
        json!({"status": s, "message": message(&body)}),
    );
    let (s, h, _) = cl
        .raw(M::GET, &format!("api/status?token={TOKEN}"), &[], None)
        .await;
    cl.record(
        "token sets login cookie",
        json!({"status": s, "cookies": cl.set_cookies(&h)}),
    );
    let login = cookie(&h, &cl.login_name());
    let (s, h, _) = cl
        .raw(M::GET, "api/kernelspecs", &[("Cookie", &login)], None)
        .await;
    cl.record(
        "cookie alone",
        json!({"status": s, "cookies": cl.set_cookies(&h)}),
    );
    let (s, h, _) = cl
        .raw(
            M::GET,
            "api/status",
            &[("Cookie", &format!("{}=forged", cl.login_name()))],
            None,
        )
        .await;
    cl.record(
        "forged cookie",
        json!({"status": s, "cookies": cl.set_cookies(&h)}),
    );

    // The login page sets `_xsrf`; the form posts it back with the token.
    let (s, h, _) = cl
        .raw(M::GET, "login?next=%2Fapi%2Fstatus", &[], None)
        .await;
    cl.record(
        "login page",
        json!({"status": s, "cookies": cl.set_cookies(&h)}),
    );
    let xsrf = cookie(&h, "_xsrf");
    let xsrf_value = xsrf.split_once('=').unwrap().1.to_string();
    let form = |password: &str, with_xsrf: bool| {
        let mut f = form_pair("password", password);
        if with_xsrf {
            f = format!("{f}&{}", form_pair("_xsrf", &xsrf_value));
        }
        Some(("application/x-www-form-urlencoded", f))
    };
    for (step, password, with_xsrf) in [
        ("login without _xsrf", TOKEN, false),
        ("login with a wrong token", "wrong", true),
    ] {
        let (s, h, _) = cl
            .raw(
                M::POST,
                "login?next=%2Fapi%2Fstatus",
                &[("Cookie", &xsrf)],
                form(password, with_xsrf),
            )
            .await;
        cl.record(step, json!({"status": s, "cookies": cl.set_cookies(&h)}));
    }
    let (s, h, _) = cl
        .raw(
            M::POST,
            "login?next=%2Fapi%2Fstatus",
            &[("Cookie", &xsrf)],
            form(TOKEN, true),
        )
        .await;
    cl.record(
        "login",
        json!({"status": s, "location": h.get("location").map(|l| l.to_str().unwrap()), "cookies": cl.set_cookies(&h)}),
    );

    // A cookie-authenticated change needs the `_xsrf` cookie echoed in a
    // header; the token alone needs nothing.
    let both = format!("{login}; {xsrf}");
    let new_file = || Some(("application/json", json!({"type": "file"}).to_string()));
    let (s, _, body) = cl
        .raw(M::POST, "api/contents", &[("Cookie", &both)], new_file())
        .await;
    cl.record(
        "cookie POST without _xsrf",
        json!({"status": s, "message": message(&body)}),
    );
    let (s, _, body) = cl
        .raw(
            M::POST,
            "api/contents",
            &[("Cookie", &both), ("X-XSRFToken", "2|00|00|0")],
            new_file(),
        )
        .await;
    cl.record(
        "cookie POST with a wrong _xsrf",
        json!({"status": s, "message": message(&body)}),
    );
    let (s, _, body) = cl
        .raw(
            M::POST,
            "api/contents",
            &[("Cookie", &both), ("X-XSRFToken", &xsrf_value)],
            new_file(),
        )
        .await;
    cl.record("cookie POST with _xsrf", json!({"status": s}));
    let path = serde_json::from_str::<Value>(&body).unwrap()["path"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = cl
        .call(M::DELETE, &format!("api/contents/{path}"), None)
        .await;
    cl.record("token DELETE", json!({"status": s}));
    login
}

fn form_pair(k: &str, v: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    format!("{}={}", enc(k), enc(v))
}

async fn scenario(base: &str, c: &Code) -> Vec<(String, Value)> {
    let mut cl = Client {
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        base: base.to_string(),
        transcript: vec![],
    };
    cl.wait_ready().await;
    use reqwest::Method as M;

    let (s, v) = cl.call(M::GET, "api/kernelspecs", None).await;
    let spec = &v["kernelspecs"][c.kernel];
    let mut spec_json = spec["spec"].clone();
    if c.native_spec_substituted {
        for k in ["argv", "kernel_protocol_version"] {
            spec_json.as_object_mut().unwrap().remove(k);
        }
    }
    cl.record(
        "kernelspecs",
        json!({"status": s, "keys": keys(&v), "spec": spec_json, "name": spec["name"], "resources": keys(&spec["resources"])}),
    );
    let login = browser_login(&mut cl).await;

    let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {"kernelspec": {"name": c.kernel, "display_name": c.kernel}}, "cells": [
        {"cell_type": "code", "id": "c1", "metadata": {}, "source": c.answer, "outputs": [], "execution_count": null}
    ]});
    let (s, v) = cl
        .call(
            M::PUT,
            "api/contents/scenario.ipynb",
            Some(json!({"type": "notebook", "content": nb})),
        )
        .await;
    cl.record(
        "contents save",
        json!({"status": s, "keys": keys(&v), "v": pick(&v, &["name", "path", "type", "format", "content", "writable", "mimetype"])}),
    );
    // Code cells come back with `trusted` from the signature store: the
    // notebook has no rich output, so it was signed on save.
    let (s, v) = cl.call(M::GET, "api/contents/scenario.ipynb", None).await;
    cl.record(
        "contents get",
        json!({"status": s, "keys": keys(&v), "v": pick(&v, &["name", "path", "type", "format", "mimetype", "writable"]), "content": v["content"]}),
    );
    trust_steps(&mut cl).await;
    let (s, v) = cl
        .call(M::GET, "api/contents/scenario.ipynb?content=0&hash=1", None)
        .await;
    cl.record(
        "contents hash",
        json!({"status": s, "v": pick(&v, &["hash", "hash_algorithm", "content", "format"])}),
    );
    let (s, v) = cl.call(M::GET, "api/contents", None).await;
    let names: Vec<Value> = v["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| pick(e, &["name", "type", "content", "format"]))
        .collect();
    cl.record(
        "contents list",
        json!({"status": s, "type": v["type"], "entries": names}),
    );
    let (s, v) = cl
        .call(M::POST, "api/contents/scenario.ipynb/checkpoints", None)
        .await;
    cl.record("checkpoint", json!({"status": s, "keys": keys(&v)}));
    let (s, v) = cl
        .call(M::GET, "api/contents/scenario.ipynb/checkpoints", None)
        .await;
    cl.record(
        "checkpoints",
        json!({"status": s, "count": v.as_array().map(|a| a.len())}),
    );

    let (s, session) = cl
        .call(
            M::POST,
            "api/sessions",
            Some(json!({"path": "scenario.ipynb", "type": "notebook", "name": "", "kernel": {"name": c.kernel}})),
        )
        .await;
    cl.record(
        "session create",
        json!({"status": s, "keys": keys(&session), "kernel keys": keys(&session["kernel"]),
               "v": pick(&session, &["path", "name", "type"]), "kernel": session["kernel"]["name"]}),
    );
    let kid = session["kernel"]["id"].as_str().unwrap().to_string();
    let sid = session["id"].as_str().unwrap().to_string();

    // The first websocket authenticates as JupyterLab's does in a browser,
    // with the login cookie and no token.
    let mut ws_req = format!(
        "{}api/kernels/{kid}/channels?session_id=e2e-session",
        base.replacen("http://", "ws://", 1)
    )
    .into_client_request()
    .unwrap();
    ws_req
        .headers_mut()
        .insert("Cookie", login.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_req).await.unwrap();

    let info = exchange(
        &mut ws,
        request("kernel_info_request", "shell", json!({})),
        None,
    )
    .await;
    // IPython's banner ends with a random tip.
    let mut info_reply = info["reply"].clone();
    info_reply["content"]
        .as_object_mut()
        .unwrap()
        .remove("banner");
    cl.record("kernel_info reply", info_reply);

    for (step, code) in [
        ("set state", c.set_state),
        ("result", c.answer),
        ("stream", c.stream),
        ("display", c.display),
        ("error", c.raise),
    ] {
        let r = exchange(
            &mut ws,
            request("execute_request", "shell", json!({"code": code, "silent": false, "store_history": true, "user_expressions": {}, "allow_stdin": true, "stop_on_error": true})),
            None,
        )
        .await;
        cl.record(step, r);
    }
    let r = exchange(
        &mut ws,
        request(
            "execute_request",
            "shell",
            json!({"code": c.input, "allow_stdin": true}),
        ),
        Some("Ada"),
    )
    .await;
    cl.record("stdin", r);

    // Interrupt over REST while a cell sleeps.
    let sleep = request("execute_request", "shell", json!({"code": c.sleep}));
    let interrupter = {
        let http = cl.http.clone();
        let url = format!("{base}api/kernels/{kid}/interrupt");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            http.post(url)
                .header("Authorization", format!("token {TOKEN}"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    let r = exchange(&mut ws, sleep, None).await;
    let status = interrupter.await.unwrap();
    cl.record(
        "interrupt",
        json!({"status": status, "reply status": r["reply"]["content"]["status"], "ename": r["reply"]["content"]["ename"]}),
    );
    let r = exchange(
        &mut ws,
        request("execute_request", "shell", json!({"code": c.read_state})),
        None,
    )
    .await;
    cl.record("state survives interrupt", r["iopub"].clone());

    let (s, v) = cl.call(M::GET, "api/kernels", None).await;
    cl.record(
        "kernels",
        json!({"status": s, "count": v.as_array().map(|a| a.len()), "keys": keys(&v[0]), "v": pick(&v[0], &["name", "execution_state"])}),
    );
    let (s, v) = cl.call(M::GET, "api/sessions", None).await;
    cl.record(
        "sessions",
        json!({"status": s, "count": v.as_array().map(|a| a.len())}),
    );

    let (s, v) = cl
        .call(M::POST, &format!("api/kernels/{kid}/restart"), None)
        .await;
    cl.record(
        "restart",
        json!({"status": s, "keys": keys(&v), "name": v["name"]}),
    );
    drop(ws);
    let ws_url = format!(
        "{}api/kernels/{kid}/channels?session_id=e2e-session-2&token={TOKEN}",
        base.replacen("http://", "ws://", 1)
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    let r = exchange(
        &mut ws,
        request("execute_request", "shell", json!({"code": c.read_state})),
        None,
    )
    .await;
    cl.record(
        "state lost on restart",
        json!({"status": r["reply"]["content"]["status"], "ename": r["reply"]["content"]["ename"], "count": r["reply"]["content"]["execution_count"]}),
    );
    drop(ws);

    let (s, _) = cl
        .call(M::DELETE, &format!("api/sessions/{sid}"), None)
        .await;
    cl.record("session delete", json!({"status": s}));
    let (s, v) = cl.call(M::GET, "api/kernels", None).await;
    cl.record("kernels after delete", json!({"status": s, "v": v}));
    let (s, _) = cl
        .call(M::DELETE, "api/contents/scenario.ipynb", None)
        .await;
    cl.record("contents delete", json!({"status": s}));

    let (s, h, _) = cl
        .raw(reqwest::Method::GET, "logout", &[("Cookie", &login)], None)
        .await;
    cl.record(
        "logout",
        json!({"status": s, "cookies": cl.set_cookies(&h)}),
    );

    cl.transcript
}

/// Notebook signing: rich output is trusted when the client says it was
/// produced in this session, or after `POST .../trust`, and not otherwise.
async fn trust_steps(cl: &mut Client) {
    use reqwest::Method as M;
    let rich = |html: &str, trusted: Option<bool>| {
        let mut metadata = json!({});
        if let Some(t) = trusted {
            metadata["trusted"] = json!(t);
        }
        json!({"type": "notebook", "content": {"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
            {"cell_type": "markdown", "id": "m1", "metadata": {}, "source": "# rich"},
            {"cell_type": "code", "id": "c1", "metadata": metadata, "source": "html()", "execution_count": 1,
             "outputs": [{"output_type": "display_data", "metadata": {},
                          "data": {"text/html": html, "text/plain": "<HTML>"}}]}
        ]}})
    };
    let path = "api/contents/rich.ipynb";
    let get_trusted = async |cl: &mut Client, step: &str| {
        let (s, v) = cl.call(M::GET, path, None).await;
        let flags: Vec<Value> = v["content"]["cells"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| c["metadata"].get("trusted").cloned().unwrap_or(Value::Null))
            .collect();
        cl.record(step, json!({"status": s, "trusted": flags}));
    };
    for (step, body) in [
        ("save untrusted html", rich("<b>a</b>", None)),
        ("save html marked untrusted", rich("<b>b</b>", Some(false))),
    ] {
        let (s, _) = cl.call(M::PUT, path, Some(body)).await;
        cl.record(step, json!({"status": s}));
        get_trusted(cl, &format!("{step}: get")).await;
    }
    let (s, v) = cl.call(M::POST, &format!("{path}/trust"), None).await;
    cl.record("trust", json!({"status": s, "body": v}));
    get_trusted(cl, "trust: get").await;
    let (s, _) = cl.call(M::GET, &format!("{path}/trust"), None).await;
    cl.record("trust GET", json!({"status": s}));
    let (s, _) = cl
        .call(M::PUT, path, Some(rich("<b>c</b>", Some(true))))
        .await;
    cl.record("save html from this session", json!({"status": s}));
    get_trusted(cl, "save html from this session: get").await;
    let (s, _) = cl.call(M::DELETE, path, None).await;
    cl.record("delete rich", json!({"status": s}));
}

/// Object key order carries no meaning in the API.
fn sort_keys(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let sorted: std::collections::BTreeMap<_, _> =
                o.iter().map(|(k, v)| (k.clone(), sort_keys(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sort_keys).collect()),
        v => v.clone(),
    }
}

fn run_both(c: &Code) {
    if !have_kernel(c.kernel) {
        return;
    }
    let Some(jupyter) = jupyter() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let reference = {
        let s = start_jupyter(&jupyter);
        rt.block_on(scenario(&s.base, c))
    };
    let ours = {
        let s = start_ganymede();
        rt.block_on(scenario(&s.base, c))
    };
    assert_eq!(reference.len(), ours.len());
    let mut diffs = Vec::new();
    for ((step, a), (_, b)) in reference.iter().zip(&ours) {
        let a = normalize(&serde_json::to_string_pretty(&sort_keys(a)).unwrap());
        let b = normalize(&serde_json::to_string_pretty(&sort_keys(b)).unwrap());
        if a != b {
            if std::env::var_os("E2E_DUMP").is_some() {
                eprintln!("== {step}\n-- reference\n{a}\n-- ganymede\n{b}");
            }
            diffs.push(format!("{step}:\n{}", first_difference(&a, &b)));
        }
    }
    assert!(
        diffs.is_empty(),
        "{}: ganymede differs from jupyter_server:\n{}",
        c.kernel,
        diffs.join("\n")
    );
}

#[test]
fn server_matches_jupyter_server_on_cdp_kernel() {
    run_both(&CODE[0]);
}

#[test]
fn server_matches_jupyter_server_on_ipykernel() {
    run_both(&CODE[1]);
}

fn ws_url(base: &str, kid: &str, session: &str) -> String {
    format!(
        "{}api/kernels/{kid}/channels?session_id={session}&token={TOKEN}",
        base.replacen("http://", "ws://", 1)
    )
}

/// Several kernels started at once through `serve`, as a frontend
/// restoring a workspace does, each with ports of its own: each answers.
#[test]
fn parallel_starts_through_serve() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for c in CODE.iter().filter(|c| have_kernel(c.kernel)) {
        let s = start_ganymede();
        rt.block_on(async {
            let cl = Client {
                http: reqwest::Client::new(),
                base: s.base.clone(),
                transcript: vec![],
            };
            cl.wait_ready().await;
            let started = futures::future::join_all((0..4).map(|_| {
                cl.call(
                    reqwest::Method::POST,
                    "api/kernels",
                    Some(json!({"name": c.kernel})),
                )
            }))
            .await;
            let ids: Vec<String> = started
                .iter()
                .map(|(st, k)| {
                    assert_eq!(*st, 201, "{}: {k}", c.kernel);
                    k["id"].as_str().unwrap().to_string()
                })
                .collect();
            let base = &s.base;
            let answers = futures::future::join_all(ids.iter().map(|kid| async move {
                let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(base, kid, kid))
                    .await
                    .unwrap();
                let r = exchange(
                    &mut ws,
                    request("execute_request", "shell", json!({"code": c.answer})),
                    None,
                )
                .await;
                r["reply"]["content"]["status"].clone()
            }))
            .await;
            assert!(
                answers.iter().all(|a| a == "ok"),
                "{}: {answers:?}",
                c.kernel
            );
            let (_, listed) = cl.call(reqwest::Method::GET, "api/kernels", None).await;
            assert_eq!(listed.as_array().unwrap().len(), 4, "{}", c.kernel);
            for kid in &ids {
                let (st, _) = cl
                    .call(reqwest::Method::DELETE, &format!("api/kernels/{kid}"), None)
                    .await;
                assert_eq!(st, 204);
            }
        });
    }
}

/// A restart whose new kernel never comes up, on a real kernel: the
/// kernelspec wraps the installed one in a script that runs it once and
/// fails every launch after that. Clients see `restarting`, then `dead`,
/// and the kernel is gone.
#[test]
fn restart_failure_on_a_real_kernel_reports_it_dead() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for c in CODE.iter().filter(|c| have_kernel(c.kernel)) {
        let roots = tempfile::tempdir().unwrap();
        let installed = ganymede_engine::Engine::builder()
            .build()
            .kernelspec(c.kernel)
            .unwrap();
        let name = format!("flaky-{}", c.kernel);
        let dir = roots.path().join("kernels").join(&name);
        std::fs::create_dir_all(&dir).unwrap();
        let mark = roots.path().join("launched");
        let mut argv = vec![
            "sh".to_string(),
            "-c".into(),
            r#"[ -e "$FLAKY_MARK" ] && exit 3; : > "$FLAKY_MARK"; exec "$@""#.into(),
            "flaky".into(),
        ];
        argv.extend(installed.argv.iter().cloned());
        let mut env = installed.env.clone();
        env.insert("FLAKY_MARK".into(), mark.display().to_string());
        let spec = json!({
            "argv": argv,
            "display_name": name,
            "language": installed.language,
            "env": env,
            "interrupt_mode": "message",
            "metadata": installed.metadata,
        });
        std::fs::write(dir.join("kernel.json"), spec.to_string()).unwrap();

        let s = start_ganymede_with(&[
            std::ffi::OsStr::new("--kernelspec-root"),
            roots.path().as_os_str(),
            std::ffi::OsStr::new("--startup-timeout"),
            std::ffi::OsStr::new("20"),
        ]);
        rt.block_on(async {
            let cl = Client {
                http: reqwest::Client::new(),
                base: s.base.clone(),
                transcript: vec![],
            };
            cl.wait_ready().await;
            let (st, k) = cl
                .call(
                    reqwest::Method::POST,
                    "api/kernels",
                    Some(json!({"name": name})),
                )
                .await;
            assert_eq!(st, 201, "{name}: {k}");
            let kid = k["id"].as_str().unwrap().to_string();
            let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&s.base, &kid, "flaky"))
                .await
                .unwrap();
            let r = exchange(
                &mut ws,
                request("execute_request", "shell", json!({"code": c.answer})),
                None,
            )
            .await;
            assert_eq!(r["reply"]["content"]["status"], "ok", "{name}: {r}");

            let (st, body) = cl
                .call(
                    reqwest::Method::POST,
                    &format!("api/kernels/{kid}/restart"),
                    None,
                )
                .await;
            assert!(st >= 400, "{name}: restart answered {st}: {body}");
            let mut states = vec![];
            while let Ok(Some(Ok(frame))) =
                tokio::time::timeout(Duration::from_secs(10), ws.next()).await
            {
                let WsMessage::Text(text) = frame else {
                    continue;
                };
                let m: Value = serde_json::from_str(&text).unwrap();
                if m["channel"] == "iopub"
                    && m["header"]["msg_type"] == "status"
                    && m["parent_header"].get("msg_id").is_none()
                {
                    states.push(
                        m["content"]["execution_state"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                    );
                }
            }
            assert_eq!(
                states.first().map(String::as_str),
                Some("restarting"),
                "{name}: {states:?}"
            );
            assert_eq!(
                states.last().map(String::as_str),
                Some("dead"),
                "{name}: {states:?}"
            );
            let (st, _) = cl
                .call(reqwest::Method::GET, &format!("api/kernels/{kid}"), None)
                .await;
            assert_eq!(st, 404, "{name}");
        });
    }
}

/// Await `f`, or fail the test naming the step that stalled.
async fn within<T>(secs: u64, step: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), f)
        .await
        .unwrap_or_else(|_| panic!("{step}: no result in {secs} s"))
}

/// Code that kills the kernel process itself, not only the runtime a cell
/// runs in: cdp-kernel relaunches a runtime that exits, so its cell kills
/// the kernel, its parent.
fn crash_code(kernel: &str) -> &'static str {
    match kernel {
        "cdp-node" => "process.kill(process.ppid, 'SIGKILL')",
        _ => "import os; os._exit(1)",
    }
}

/// What a client sees when its kernel crashes mid-cell: `status:
/// restarting`, then the same kernel id runs again with its state gone.
/// Returns the crashed cell's reply, if one came.
async fn crash(base: &str, c: &Code, wait_idle: bool) -> Option<Value> {
    let cl = Client {
        // No client timeout: `within` bounds each call and names the step.
        http: reqwest::Client::new(),
        base: base.to_string(),
        transcript: vec![],
    };
    within(60, "waiting for the server", cl.wait_ready()).await;
    use reqwest::Method as M;
    let (s, k) = within(
        60,
        "starting the kernel",
        cl.call(M::POST, "api/kernels", Some(json!({"name": c.kernel}))),
    )
    .await;
    assert_eq!(s, 201, "{k}");
    let kid = k["id"].as_str().unwrap().to_string();
    let ws_url = |session: &str| {
        format!(
            "{}api/kernels/{kid}/channels?session_id={session}&token={TOKEN}",
            base.replacen("http://", "ws://", 1)
        )
    };
    let (mut ws, _) = within(
        30,
        "connecting the first websocket",
        tokio_tungstenite::connect_async(ws_url("crash-1")),
    )
    .await
    .unwrap();
    let r = within(
        180,
        "running the cell that sets state",
        exchange_step(
            "running the cell that sets state",
            &mut ws,
            request("execute_request", "shell", json!({"code": c.set_state})),
            None,
        ),
    )
    .await;
    assert_eq!(r["reply"]["content"]["status"], "ok", "{r}");

    let cell = request(
        "execute_request",
        "shell",
        json!({"code": crash_code(c.kernel)}),
    );
    let cell_id = cell["header"]["msg_id"].clone();
    within(
        30,
        "sending the crash cell",
        ws.send(WsMessage::text(cell.to_string())),
    )
    .await
    .unwrap();
    let mut reply = None;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(30), ws.next())
            .await
            .expect("no restarting status in 30 s");
        let Some(Ok(WsMessage::Text(text))) = frame else {
            panic!("websocket closed before the kernel restarted: {frame:?}");
        };
        let m: Value = serde_json::from_str(&text).unwrap();
        if m["channel"] == "shell" && m["parent_header"]["msg_id"] == cell_id {
            reply = Some(m["content"].clone());
        }
        if m["channel"] == "iopub"
            && m["header"]["msg_type"] == "status"
            && m["content"]["execution_state"] == "restarting"
        {
            break;
        }
    }
    drop(ws);

    // The same id, with a fresh kernel behind it. Ganymede reports it
    // idle once it answers. jupyter_server can leave it `busy`, from the
    // crashed cell, when the new kernel publishes no status of its own
    // (cdp-kernel), so there the next cell is what shows it is back.
    let (s, v) = within(
        60,
        "reading the kernel after the crash",
        cl.call(M::GET, &format!("api/kernels/{kid}"), None),
    )
    .await;
    assert_eq!(s, 200, "kernel removed instead of restarted: {v}");
    if wait_idle {
        let mut state = Value::Null;
        for _ in 0..300 {
            let (_, v) = within(
                60,
                "polling the restarted kernel",
                cl.call(M::GET, &format!("api/kernels/{kid}"), None),
            )
            .await;
            state = v["execution_state"].clone();
            if state == "idle" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(state, "idle");
    }
    let (mut ws, _) = within(
        30,
        "connecting the second websocket",
        tokio_tungstenite::connect_async(ws_url("crash-2")),
    )
    .await
    .unwrap();
    let r = within(
        180,
        "running a cell on the restarted kernel",
        exchange_step(
            "running a cell on the restarted kernel",
            &mut ws,
            request("execute_request", "shell", json!({"code": c.read_state})),
            None,
        ),
    )
    .await;
    assert_eq!(
        r["reply"]["content"]["status"], "error",
        "state survived: {r}"
    );
    drop(ws);
    let (s, _) = within(
        60,
        "deleting the kernel",
        cl.call(M::DELETE, &format!("api/kernels/{kid}"), None),
    )
    .await;
    assert_eq!(s, 204);
    reply
}

fn crash_both(c: &Code) {
    if !have_kernel(c.kernel) {
        return;
    }
    let Some(jupyter) = jupyter() else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    {
        let s = start_jupyter(&jupyter);
        rt.block_on(crash(&s.base, c, false));
    }
    let s = start_ganymede();
    // jupyter_server leaves the crashed cell unanswered; Ganymede answers
    // it, so the client stops waiting.
    let reply = rt
        .block_on(crash(&s.base, c, true))
        .expect("the crashed cell is answered");
    assert_eq!(
        (&reply["status"], &reply["ename"]),
        (&json!("error"), &json!("KernelDied")),
        "{reply}"
    );
}

#[test]
fn a_crashed_cdp_kernel_restarts_as_under_jupyter_server() {
    crash_both(&CODE[0]);
}

#[test]
fn a_crashed_ipykernel_restarts_as_under_jupyter_server() {
    crash_both(&CODE[1]);
}
