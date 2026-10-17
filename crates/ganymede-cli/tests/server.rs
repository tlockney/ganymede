//! The server against the reference: one scenario, driven over HTTP and
//! the websocket exactly as a frontend would, against `ganymede serve` and
//! against `jupyter server` (jupyter_server) on the same kernelspec. Every
//! step records what a client can observe (status codes, model keys and
//! values, the iopub messages each request produced, reply contents); the
//! two transcripts must be equal once timestamps and random ids are
//! normalized.

mod common;

use common::*;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
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
    fn drop(&mut self) {
        unsafe {
            kill(self.child.id() as i32, 15);
        }
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
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut child = ganymede(&["serve", "--port", "0", "--token", TOKEN, "--root-dir"])
        .arg(root.path())
        .arg("--config-dir")
        .arg(state.path())
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
    let child = Command::new(jupyter)
        .args([
            "server",
            "--no-browser",
            "--allow-root",
            &format!("--ServerApp.port={port}"),
            "--ServerApp.port_retries=0",
            &format!("--IdentityProvider.token={TOKEN}"),
        ])
        .arg(format!("--ServerApp.root_dir={}", root.path().display()))
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
    let id = msg["header"]["msg_id"].as_str().unwrap().to_string();
    ws.send(WsMessage::text(msg.to_string())).await.unwrap();
    let mut iopub = Vec::new();
    let mut stdin = Vec::new();
    let mut reply = None;
    let mut idle = false;
    while reply.is_none() || !idle {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .expect("no message in 60 s")
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

async fn scenario(base: &str, c: &Code) -> Vec<(String, Value)> {
    let mut cl = Client {
        http: reqwest::Client::new(),
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
    let (s, mut v) = cl.call(M::GET, "api/contents/scenario.ipynb", None).await;
    // jupyter_server marks code cells `trusted` from its notebook-signature
    // store; Ganymede has no trust store yet. Compared without it.
    for cell in v["content"]["cells"].as_array_mut().into_iter().flatten() {
        cell["metadata"]
            .as_object_mut()
            .map(|m| m.remove("trusted"));
    }
    cl.record(
        "contents get",
        json!({"status": s, "keys": keys(&v), "v": pick(&v, &["name", "path", "type", "format", "mimetype", "writable"]), "content": v["content"]}),
    );
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

    let ws_url = format!(
        "{}api/kernels/{kid}/channels?session_id=e2e-session&token={TOKEN}",
        base.replacen("http://", "ws://", 1)
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();

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

    cl.transcript
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
