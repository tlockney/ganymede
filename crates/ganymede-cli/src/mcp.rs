//! `ganymede mcp`: the server's MCP tools (`/mcp`) over stdio, for agents
//! that start MCP servers as subprocesses (Claude Code, Codex, Cursor).
//!
//! It forwards each JSON-RPC message on stdin to a server's `/mcp` and
//! writes the answers to stdout, so the agent's edits go through the same
//! document rooms as a person's JupyterLab or ganymede-ui. The server is
//! `--server`, else a running one (from its `jpserver-<pid>.json`) whose
//! root holds the root dir and that serves `/mcp`, else one started in this
//! process on a loopback port with a token of its own.
//!
//! A server of its own is announced in `ganymede-mcp-<pid>.json` in the
//! Jupyter runtime dir while it runs, for `ganymede mcp call` only: other
//! `ganymede mcp` processes don't use it, since it stops with its agent.
//!
//! `ganymede mcp call TOOL [ARGS]` calls one tool on the server
//! `ganymede mcp` would use, for scripts and agent hooks, and prints its
//! text.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = false)]
pub struct McpArgs {
    #[command(subcommand)]
    pub cmd: Option<McpCmd>,
    /// The notebooks' directory. Default: the current directory.
    #[arg(long, global = true)]
    pub root_dir: Option<PathBuf>,
    /// Use this server (its URL with the base URL, such as
    /// `http://localhost:8888/`) instead of finding one.
    #[arg(long, value_name = "URL", global = true)]
    pub server: Option<String>,
    /// The token for `--server`.
    #[arg(long, env = "JUPYTER_TOKEN", hide_env_values = true, global = true)]
    pub token: Option<String>,
    /// Start a server in this process even when one is running.
    #[arg(long)]
    pub own_server: bool,
    /// Read notebooks only: no edits, no execution. Implies --own-server.
    #[arg(long)]
    pub read_only: bool,
    /// Edit notebooks but run no code. Implies --own-server.
    #[arg(long)]
    pub no_execute: bool,
    /// Send images in outputs to the model. Implies --own-server.
    #[arg(long)]
    pub images: bool,
    /// Kernelspec for new notebooks, on a server this command starts.
    #[arg(long, value_name = "NAME")]
    pub default_kernel: Option<String>,
}

#[derive(clap::Subcommand)]
pub enum McpCmd {
    /// Call one tool and print its text, for scripts and agent hooks.
    ///
    /// Uses `--server`, else a running `ganymede serve --mcp` whose root
    /// holds the root dir, else a server a `ganymede mcp` started for
    /// itself there (the most recent). Starts none: with no server it
    /// fails. Exits 1 when the tool reports an error.
    Call(CallArgs),
}

#[derive(clap::Args)]
pub struct CallArgs {
    /// The tool, such as `notebook_read`.
    pub tool: String,
    /// Its arguments, as a JSON object.
    #[arg(default_value = "{}")]
    pub args: String,
    /// The client name the server records in provenance.
    #[arg(long, default_value = "ganymede-mcp-call")]
    pub client: String,
}

/// A server's `/mcp` endpoint and token.
#[derive(Clone)]
struct Endpoint {
    url: String,
    token: Option<String>,
    /// The agent's folder relative to the server's root, when the server
    /// is rooted above it: sent on `initialize` so the agent's relative
    /// paths start there (`ganymede_server::mcp::DIR_META`).
    dir: Option<String>,
}

/// `root` relative to `server_root` (which holds it), as an API path;
/// `None` when they are the same directory.
fn dir_under(root: &Path, server_root: &Path) -> Option<String> {
    let rest = root.strip_prefix(server_root).ok()?;
    let dir = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    (!dir.is_empty()).then_some(dir)
}

pub async fn run(
    args: McpArgs,
    kernelspec_roots: &[PathBuf],
    startup_timeout: Option<u64>,
) -> Result<()> {
    let root = match &args.root_dir {
        Some(r) => r.clone(),
        None => std::env::current_dir()?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("root dir {}", root.display()))?;
    if let Some(McpCmd::Call(call_args)) = &args.cmd {
        let ep = match &args.server {
            Some(server) => Some(Endpoint {
                url: mcp_url(server),
                token: args.token.clone(),
                dir: None,
            }),
            None => {
                let root = root.clone();
                tokio::task::spawn_blocking(move || find_server(&root).or_else(|| find_own(&root)))
                    .await?
            }
        };
        let Some(ep) = ep else {
            bail!(
                "no server for {}: start `ganymede serve --mcp`, or an agent's `ganymede mcp`",
                root.display()
            );
        };
        let (tool, client) = (call_args.tool.clone(), call_args.client.clone());
        let tool_args: Value =
            serde_json::from_str(&call_args.args).context("ARGS must be a JSON object")?;
        if !tool_args.is_object() {
            bail!("ARGS must be a JSON object");
        }
        let (text, is_error) =
            tokio::task::spawn_blocking(move || call(&ep, &client, &tool, tool_args)).await??;
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        if is_error {
            std::process::exit(1);
        }
        return Ok(());
    }
    let own = args.own_server || args.read_only || args.no_execute || args.images;
    if let Some(server) = &args.server {
        if own {
            bail!("--server can't be combined with --own-server, --read-only, --no-execute or --images: a running server's own settings apply");
        }
        let ep = Endpoint {
            url: mcp_url(server),
            token: args.token.clone(),
            dir: None,
        };
        return tokio::task::spawn_blocking(move || proxy(ep)).await?;
    }
    if !own {
        let found = tokio::task::spawn_blocking({
            let root = root.clone();
            move || find_server(&root)
        })
        .await?;
        if let Some(ep) = found {
            tracing::info!("using the server at {}", ep.url);
            return tokio::task::spawn_blocking(move || proxy(ep)).await?;
        }
    }

    // A server of our own, stopped when stdin closes.
    let mut cfg = ganymede_server::serve::ServeArgs {
        root_dir: Some(root.clone()),
        default_kernel: args.default_kernel.clone(),
        kernelspec_root: kernelspec_roots.to_vec(),
        ..Default::default()
    }
    .into_config()?;
    cfg.ip = "127.0.0.1".into();
    cfg.port = 0;
    cfg.base_url = "/".into();
    cfg.token = Some(uuid::Uuid::new_v4().simple().to_string());
    cfg.allow_remote_access = Some(false);
    cfg.mcp_enabled = true;
    cfg.mcp_read_only = args.read_only;
    cfg.mcp_no_execute = args.no_execute;
    cfg.mcp_images = args.images;
    cfg.quiet = true;
    if let Some(t) = startup_timeout {
        cfg.startup_timeout_secs = t;
    }
    let token = cfg.token.clone();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(2);
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
    let startup = ganymede_server::serve::Startup {
        stop: Some(stop_rx),
        on_listening: Some(Box::new(move |url: &str| {
            let _ = url_tx.send(url.to_string());
        })),
        ..Default::default()
    };
    let mut server = tokio::spawn(ganymede_server::serve::run_with(cfg, startup));
    let url = tokio::select! {
        url = url_rx => url.context("the server stopped before listening")?,
        r = &mut server => {
            r??;
            bail!("the server stopped before listening");
        }
    };
    // `http://127.0.0.1:PORT/?token=...`
    let base = url.split('?').next().unwrap_or(&url).to_string();
    let ep = Endpoint {
        url: mcp_url(&base),
        token,
        dir: None,
    };
    let announced = announce(&base, ep.token.as_deref(), root);
    let proxying = tokio::task::spawn_blocking(move || proxy(ep));
    // Ctrl-C or SIGTERM stops the server too, which saves open notebooks
    // and shuts its kernels down; stdin closing does the same.
    let r = tokio::select! {
        r = proxying => Some(r?),
        () = stop_signal() => None,
    };
    if let Some(f) = &announced {
        let _ = std::fs::remove_file(f);
    }
    let _ = stop_tx.send(()).await;
    server.await??;
    match r {
        Some(r) => r,
        // The proxy thread is still reading stdin, which would keep the
        // runtime from shutting down.
        None => std::process::exit(130),
    }
}

/// The first Ctrl-C, or on Unix SIGTERM or SIGHUP.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = hup.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Writes `ganymede-mcp-<pid>.json` (`url`, `token`, `root_dir`, `pid`),
/// readable only by the user, for [`find_own`]. Best effort.
fn announce(base: &str, token: Option<&str>, root: PathBuf) -> Option<PathBuf> {
    let dir = ganymede_engine::host::kernelspec::jupyter_runtime_dir();
    let path = dir.join(format!("ganymede-mcp-{}.json", std::process::id()));
    let info = json!({"url": base, "token": token, "root_dir": root, "pid": std::process::id()});
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&path)?.write_all(info.to_string().as_bytes())
    };
    match write() {
        Ok(()) => Some(path),
        Err(e) => {
            tracing::debug!("announcing the server in {}: {e}", path.display());
            None
        }
    }
}

/// A server a `ganymede mcp` started for itself, whose root holds `root`
/// and that still answers: the deepest root, then the newest.
fn find_own(root: &Path) -> Option<Endpoint> {
    let dir = ganymede_engine::host::kernelspec::jupyter_runtime_dir();
    let mut found: Vec<((usize, std::time::SystemTime), Endpoint)> = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("ganymede-mcp-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(info) = std::fs::read_to_string(entry.path())
            .map_err(|_| ())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|_| ()))
        else {
            continue;
        };
        let (Some(url), Some(server_root)) = (info["url"].as_str(), info["root_dir"].as_str())
        else {
            continue;
        };
        let Ok(server_root) = Path::new(server_root).canonicalize() else {
            continue;
        };
        if !root.starts_with(&server_root) {
            continue;
        }
        let ep = Endpoint {
            url: mcp_url(url),
            token: info["token"].as_str().map(String::from),
            dir: dir_under(root, &server_root),
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        if serves_mcp(&ep) {
            found.push(((server_root.components().count(), modified), ep));
        }
    }
    found.sort_by_key(|(key, _)| std::cmp::Reverse(*key));
    found.into_iter().next().map(|(_, ep)| ep)
}

/// Calls `tool` in a session of its own: its text, and whether the tool
/// reported an error.
fn call(ep: &Endpoint, client: &str, tool: &str, args: Value) -> Result<(String, bool)> {
    let agent = agent();
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": client, "version": env!("CARGO_PKG_VERSION")}}});
    let id = start(&agent, ep, &init, None)?;
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": tool, "arguments": args}});
    let answer = (|| -> Result<Value> {
        let mut resp = send_with(&agent, ep, &msg, Some(&id), None)?;
        let status = resp.status();
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let reader = resp.body_mut().as_reader();
        if is_sse {
            let mut answer = None;
            relay_events(reader, Some(&json!(1)), |v| {
                if v["id"] == 1 && v.get("method").is_none() {
                    answer = Some(v.clone());
                }
            })?;
            return answer.context("no answer");
        }
        let mut body = String::new();
        reader.take(64 << 20).read_to_string(&mut body)?;
        serde_json::from_str::<Value>(&body).with_context(|| format!("{status}: {}", body.trim()))
    })();
    let mut req = agent.delete(&ep.url).header("Mcp-Session-Id", &id);
    if let Some(t) = &ep.token {
        req = req.header("Authorization", &format!("token {t}"));
    }
    let _ = req.call();
    let answer = answer?;
    if let Some(e) = answer.get("error") {
        return Ok((e["message"].as_str().unwrap_or("error").to_string(), true));
    }
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Ok((text, result["isError"] == true))
}

fn mcp_url(base: &str) -> String {
    format!("{}/mcp", base.trim_end_matches('/'))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        // No proxy: these are servers on this machine.
        .proxy(None)
        .build()
        .into()
}

/// A running server whose root holds `root` and that serves `/mcp`; the
/// one with the deepest root.
fn find_server(root: &Path) -> Option<Endpoint> {
    let dir = ganymede_engine::host::kernelspec::jupyter_runtime_dir();
    let mut found: Vec<(usize, Endpoint)> = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("jpserver-") && name.ends_with(".json")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let (Some(url), Some(server_root)) = (info["url"].as_str(), info["root_dir"].as_str())
        else {
            continue;
        };
        let Ok(server_root) = Path::new(server_root).canonicalize() else {
            continue;
        };
        if !root.starts_with(&server_root) {
            continue;
        }
        let ep = Endpoint {
            url: mcp_url(url),
            token: info["token"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(String::from),
            dir: dir_under(root, &server_root),
        };
        if serves_mcp(&ep) {
            found.push((server_root.components().count(), ep));
        }
    }
    found.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    found.into_iter().next().map(|(_, ep)| ep)
}

/// Whether the server answers at `/mcp`: a `GET` without a session is
/// refused by rmcp, but not with the 404 of a server without `/mcp`.
fn serves_mcp(ep: &Endpoint) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_global(Some(std::time::Duration::from_secs(2)))
        .build()
        .into();
    let mut req = agent.get(&ep.url).header("Accept", "text/event-stream");
    if let Some(t) = &ep.token {
        req = req.header("Authorization", &format!("token {t}"));
    }
    match req.call() {
        Ok(r) => !matches!(r.status().as_u16(), 401 | 403 | 404),
        Err(_) => false,
    }
}

/// The session, once `initialize` has set it.
#[derive(Default)]
struct Session {
    id: Option<String>,
    version: Option<String>,
    /// The client's `initialize`, to start a new session with when the
    /// server has ended ours.
    init: Option<Value>,
    /// How many times starting a new session has failed.
    failed_starts: u64,
}

/// Forwards stdin to `ep` until stdin closes, then ends the session.
fn proxy(ep: Endpoint) -> Result<()> {
    let agent = agent();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let session = Arc::new(Mutex::new(Session::default()));
    let stdin = std::io::stdin();
    let order = Arc::new(Order::default());
    let mut next = 0u64;
    let mut workers = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_line(
                    &out,
                    &json!({"jsonrpc": "2.0", "id": null,
                            "error": {"code": -32700, "message": format!("parse error: {e}")}}),
                );
                continue;
            }
        };
        // Messages reach the server in the order they came (each waits for
        // the one before to be accepted), but their answers stream back
        // alongside, so a long cell doesn't hold up a cancellation.
        // `initialize` sets the session the rest need, so it is answered
        // before the next line is read.
        let ticket = Ticket {
            order: order.clone(),
            n: next,
            done: false,
        };
        next += 1;
        if msg["method"] == "initialize" {
            forward(&agent, &ep, &session, &out, msg, ticket);
        } else {
            let (agent, ep, session, out) =
                (agent.clone(), ep.clone(), session.clone(), out.clone());
            workers.push(std::thread::spawn(move || {
                forward(&agent, &ep, &session, &out, msg, ticket)
            }));
            workers.retain(|w| !w.is_finished());
        }
    }
    // Answer what is still running before ending the session.
    for w in workers {
        let _ = w.join();
    }
    let id = session.lock().expect("session").id.clone();
    if let Some(id) = id {
        let mut req = agent.delete(&ep.url).header("Mcp-Session-Id", &id);
        if let Some(t) = &ep.token {
            req = req.header("Authorization", &format!("token {t}"));
        }
        let _ = req.call();
    }
    Ok(())
}

/// The order messages are posted in.
#[derive(Default)]
struct Order {
    turn: Mutex<u64>,
    cv: std::sync::Condvar,
}

/// One message's place in [`Order`]; dropping it lets the next go.
struct Ticket {
    order: Arc<Order>,
    n: u64,
    done: bool,
}

impl Ticket {
    fn wait(&self) {
        let mut turn = self.order.turn.lock().expect("order");
        while *turn < self.n {
            turn = self.order.cv.wait(turn).expect("order");
        }
    }

    fn release(&mut self) {
        if !self.done {
            self.done = true;
            let mut turn = self.order.turn.lock().expect("order");
            *turn = (*turn).max(self.n + 1);
            self.order.cv.notify_all();
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.release();
    }
}

fn write_line(out: &Mutex<std::io::Stdout>, v: &Value) {
    let mut out = out.lock().expect("stdout");
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// Posts one message and writes what comes back: a JSON body, or each
/// message of an event stream. A failure becomes a JSON-RPC error for a
/// request, and a log line for a notification.
fn forward(
    agent: &ureq::Agent,
    ep: &Endpoint,
    session: &Mutex<Session>,
    out: &Mutex<std::io::Stdout>,
    msg: Value,
    ticket: Ticket,
) {
    let id = msg
        .get("id")
        .cloned()
        .filter(|_| msg.get("method").is_some());
    if let Err(e) = post(agent, ep, session, out, &msg, ticket) {
        match id {
            Some(id) => write_line(
                out,
                &json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32603, "message": format!("ganymede mcp: {e:#}")}}),
            ),
            None => tracing::warn!("forwarding to {}: {e:#}", ep.url),
        }
    }
}

fn post(
    agent: &ureq::Agent,
    ep: &Endpoint,
    session: &Mutex<Session>,
    out: &Mutex<std::io::Stdout>,
    msg: &Value,
    mut ticket: Ticket,
) -> Result<()> {
    ticket.wait();
    if msg["method"] == "initialize" {
        session.lock().expect("session").init = Some(msg.clone());
    }
    let (resp, sent) = send(agent, ep, session, msg);
    // Accepted (its headers are back), or failed: the next may go.
    ticket.release();
    let mut resp = resp?;
    // The server ended our session (rmcp does after an idle while, and a
    // restarted server knows none): start a new one and try once more.
    if resp.status().as_u16() == 404 && sent.is_some() && msg["method"] != "initialize" {
        reinitialize(agent, ep, session, sent.as_deref())?;
        resp = send(agent, ep, session, msg).0?;
    }
    let status = resp.status();
    if let Some(id) = resp.headers().get("mcp-session-id") {
        session.lock().expect("session").id = id.to_str().ok().map(String::from);
    }
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let reader = resp.body_mut().as_reader();
    if is_sse {
        let want = msg.get("id").filter(|_| msg.get("method").is_some());
        return relay_events(reader, want, |v| {
            note_version(session, v);
            write_line(out, v);
        });
    }
    let mut body = String::new();
    reader.take(64 << 20).read_to_string(&mut body)?;
    if status.as_u16() == 202 || body.trim().is_empty() {
        if status.is_success() {
            return Ok(());
        }
        bail!("{status}");
    }
    match serde_json::from_str::<Value>(&body) {
        Ok(v) if v.get("jsonrpc").is_some() => {
            note_version(session, &v);
            write_line(out, &v);
            Ok(())
        }
        _ => bail!("{status}: {}", body.trim()),
    }
}

/// Posts `msg` with the session's headers (none on an `initialize`, which
/// starts a session); also returns the session id it went with.
fn send(
    agent: &ureq::Agent,
    ep: &Endpoint,
    session: &Mutex<Session>,
    msg: &Value,
) -> (
    Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    Option<String>,
) {
    let (id, version) = if msg["method"] == "initialize" {
        (None, None)
    } else {
        let s = session.lock().expect("session");
        (s.id.clone(), s.version.clone())
    };
    let resp = send_with(agent, ep, msg, id.as_deref(), version.as_deref());
    (resp, id)
}

/// Posts `msg` with these session headers.
fn send_with(
    agent: &ureq::Agent,
    ep: &Endpoint,
    msg: &Value,
    id: Option<&str>,
    version: Option<&str>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let mut req = agent
        .post(&ep.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(t) = &ep.token {
        req = req.header("Authorization", &format!("token {t}"));
    }
    if let Some(id) = id {
        req = req.header("Mcp-Session-Id", id);
    }
    if let Some(v) = version {
        req = req.header("MCP-Protocol-Version", v);
    }
    if let (Some(dir), Some("initialize")) = (&ep.dir, msg["method"].as_str()) {
        let mut msg = msg.clone();
        if let Some(params) = msg.get_mut("params").and_then(Value::as_object_mut) {
            let meta = params.entry("_meta").or_insert_with(|| json!({}));
            if let Some(meta) = meta.as_object_mut() {
                meta.insert(
                    ganymede_server::mcp::DIR_META.to_string(),
                    Value::String(dir.clone()),
                );
            }
        }
        return req.send(msg.to_string());
    }
    req.send(msg.to_string())
}

/// Starts a new session in place of `stale` with the client's own
/// `initialize`, unless another message already did. The answers stay
/// here: the client still has the first ones.
///
/// The stale id stays in place until the new session is ready (its
/// `initialized` sent): messages sent meanwhile get a 404 and wait here.
/// If starting it fails, those waiting fail too, rather than each trying
/// again in turn; the next message's 404 tries again.
fn reinitialize(
    agent: &ureq::Agent,
    ep: &Endpoint,
    session: &Mutex<Session>,
    stale: Option<&str>,
) -> Result<()> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let failures = session.lock().expect("session").failed_starts;
    let _one = ONE_AT_A_TIME.lock().expect("reinitialize");
    let (init, version) = {
        let s = session.lock().expect("session");
        if s.id.as_deref() != stale {
            return Ok(());
        }
        if s.failed_starts != failures {
            bail!("starting a new session failed");
        }
        let init = s
            .init
            .clone()
            .context("the server ended the session before it began")?;
        (init, s.version.clone())
    };
    tracing::info!("the server ended the MCP session; starting a new one");
    let r = start(agent, ep, &init, version.as_deref());
    let mut s = session.lock().expect("session");
    match r {
        Ok(id) => {
            s.id = Some(id);
            Ok(())
        }
        Err(e) => {
            s.failed_starts += 1;
            Err(e)
        }
    }
}

/// Sends `init` and `notifications/initialized`; returns the new session
/// id.
fn start(
    agent: &ureq::Agent,
    ep: &Endpoint,
    init: &Value,
    version: Option<&str>,
) -> Result<String> {
    let mut resp = send_with(agent, ep, init, None, None)?;
    if !resp.status().is_success() {
        bail!("starting a new session: {}", resp.status());
    }
    let id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .context("starting a new session: no session id")?;
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let reader = resp.body_mut().as_reader();
    if is_sse {
        relay_events(reader, init.get("id"), |_| {})?;
    } else {
        std::io::copy(&mut reader.take(1 << 20), &mut std::io::sink())?;
    }
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let resp = send_with(agent, ep, &initialized, Some(&id), version)?;
    if !resp.status().is_success() {
        bail!("starting a new session: {}", resp.status());
    }
    Ok(id)
}

/// Passes on each message of an event stream. A request (`want` is its
/// id) is answered by a message with that id; a stream that ends before
/// that (the server stopped, the connection dropped) is an error, so the
/// client gets one instead of waiting forever.
fn relay_events(
    reader: impl Read,
    want: Option<&Value>,
    mut emit: impl FnMut(&Value),
) -> Result<()> {
    let mut answered = want.is_none();
    let read = || -> Result<()> {
        for line in BufReader::new(reader).lines() {
            let line = line?;
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data.is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(data).context("bad event from the server")?;
                emit(&v);
                if want.is_some_and(|id| v.get("id") == Some(id) && v.get("method").is_none()) {
                    answered = true;
                }
            }
        }
        Ok(())
    };
    let r = read();
    if answered {
        return Ok(());
    }
    r?;
    bail!("the server's event stream ended without an answer")
}

/// The protocol version `initialize` settled on, sent on later requests.
fn note_version(session: &Mutex<Session>, v: &Value) {
    if let Some(ver) = v["result"]["protocolVersion"].as_str() {
        session.lock().expect("session").version = Some(ver.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay(stream: &str, want: Option<Value>) -> (Result<()>, Vec<Value>) {
        let mut got = Vec::new();
        let r = relay_events(stream.as_bytes(), want.as_ref(), |v| got.push(v.clone()));
        (r, got)
    }

    #[test]
    fn a_stream_without_the_answer_is_an_error() {
        let progress =
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n";
        let (r, got) = relay(progress, Some(json!(7)));
        assert!(r.unwrap_err().to_string().contains("without an answer"));
        assert_eq!(got.len(), 1);

        // A server request with the same id is not the answer.
        let ping = "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n\n";
        assert!(relay(ping, Some(json!(7))).0.is_err());

        let answer =
            format!("{progress}data: {{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{{}}}}\n\n");
        let (r, got) = relay(&answer, Some(json!(7)));
        r.unwrap();
        assert_eq!(got.len(), 2);

        // A notification wants no answer.
        relay(progress, None).0.unwrap();
    }
}
