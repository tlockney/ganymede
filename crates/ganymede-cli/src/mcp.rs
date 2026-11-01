//! `ganymede mcp`: the server's MCP tools (`/mcp`) over stdio, for agents
//! that start MCP servers as subprocesses (Claude Code, Codex, Cursor).
//!
//! It forwards each JSON-RPC message on stdin to a server's `/mcp` and
//! writes the answers to stdout, so the agent's edits go through the same
//! document rooms as a person's JupyterLab or ganymede-ui. The server is
//! `--server`, else a running one (from its `jpserver-<pid>.json`) whose
//! root holds the root dir and that serves `/mcp`, else one started in this
//! process on a loopback port with a token of its own.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(clap::Args)]
pub struct McpArgs {
    /// The notebooks' directory. Default: the current directory.
    #[arg(long)]
    pub root_dir: Option<PathBuf>,
    /// Use this server (its URL with the base URL, such as
    /// `http://localhost:8888/`) instead of finding one.
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,
    /// The token for `--server`.
    #[arg(long, env = "JUPYTER_TOKEN", hide_env_values = true)]
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

/// A server's `/mcp` endpoint and token.
#[derive(Clone)]
struct Endpoint {
    url: String,
    token: Option<String>,
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
    let own = args.own_server || args.read_only || args.no_execute || args.images;
    if let Some(server) = &args.server {
        if own {
            bail!("--server can't be combined with --own-server, --read-only, --no-execute or --images: a running server's own settings apply");
        }
        let ep = Endpoint {
            url: mcp_url(server),
            token: args.token.clone(),
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
        root_dir: Some(root),
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
    };
    let r = tokio::task::spawn_blocking(move || proxy(ep)).await?;
    // The server saves open notebooks as it stops.
    let _ = stop_tx.send(()).await;
    server.await??;
    r
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
}

/// Forwards stdin to `ep` until stdin closes, then ends the session.
fn proxy(ep: Endpoint) -> Result<()> {
    let agent = agent();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let session = Arc::new(Mutex::new(Session::default()));
    let stdin = std::io::stdin();
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
        // `initialize` sets the session the rest need, so it is answered
        // before the next line is read; everything else runs alongside,
        // so a long cell doesn't hold up a cancellation.
        if msg["method"] == "initialize" {
            forward(&agent, &ep, &session, &out, msg);
        } else {
            let (agent, ep, session, out) =
                (agent.clone(), ep.clone(), session.clone(), out.clone());
            workers.push(std::thread::spawn(move || {
                forward(&agent, &ep, &session, &out, msg)
            }));
            workers.retain(|w| !w.is_finished());
        }
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
) {
    let id = msg
        .get("id")
        .cloned()
        .filter(|_| msg.get("method").is_some());
    if let Err(e) = post(agent, ep, session, out, &msg) {
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
) -> Result<()> {
    let mut req = agent
        .post(&ep.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(t) = &ep.token {
        req = req.header("Authorization", &format!("token {t}"));
    }
    {
        let s = session.lock().expect("session");
        if let Some(id) = &s.id {
            req = req.header("Mcp-Session-Id", id);
        }
        if let Some(v) = &s.version {
            req = req.header("MCP-Protocol-Version", v);
        }
    }
    let mut resp = req.send(msg.to_string())?;
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
        for line in BufReader::new(reader).lines() {
            let line = line?;
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data.is_empty() {
                    continue;
                }
                let v: Value = serde_json::from_str(data).context("bad event from the server")?;
                note_version(session, &v);
                write_line(out, &v);
            }
        }
        return Ok(());
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

/// The protocol version `initialize` settled on, sent on later requests.
fn note_version(session: &Mutex<Session>, v: &Value) {
    if let Some(ver) = v["result"]["protocolVersion"].as_str() {
        session.lock().expect("session").version = Some(ver.to_string());
    }
}
