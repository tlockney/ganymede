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
//! text. `ganymede mcp watch` keeps running and prints a line for each
//! new request a person writes to the agent in a notebook, for a plugin
//! monitor that wakes the agent.

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
    /// Print each request a person writes to the agent, and keep running.
    ///
    /// For a Claude Code plugin monitor, which passes each line to the
    /// agent.
    /// Asks the server `call` would use for its open requests (Markdown
    /// cells starting `@agent` or `@` and the client's name, written by a
    /// person and not yet answered) every `--every` seconds, and prints a
    /// request once its text has stayed the same for `--settle` seconds,
    /// so a person still typing isn't interrupted. Waits for a server
    /// when there is none.
    Watch(WatchArgs),
}

#[derive(clap::Args)]
pub struct WatchArgs {
    /// The agent's client name: requests may name it (`@claude` for
    /// `claude-code`).
    #[arg(long, default_value = "claude-code")]
    pub client: String,
    /// Seconds between checks.
    #[arg(long, default_value_t = 2)]
    pub every: u64,
    /// Seconds a request must stay unchanged before it is printed.
    #[arg(long, default_value_t = 3)]
    pub settle: u64,
    /// Also print a line when a person's edits to a notebook settle (a
    /// spike, off by default).
    #[arg(long)]
    pub edits: bool,
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
    if let Some(McpCmd::Watch(w)) = &args.cmd {
        let (server, token) = (args.server.clone(), args.token.clone());
        let (client, every, settle, edits) = (w.client.clone(), w.every.max(1), w.settle, w.edits);
        return tokio::task::spawn_blocking(move || {
            watch(&root, server, token, &client, every, settle, edits)
        })
        .await?;
    }
    if let Some(McpCmd::Call(call_args)) = &args.cmd {
        let (at, server, token) = (root.clone(), args.server.clone(), args.token.clone());
        let ep =
            tokio::task::spawn_blocking(move || endpoint(&at, server.as_deref(), token.as_ref()))
                .await?;
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
    call_with(&agent(), ep, client, tool, args)
}

/// [`call`] through `agent`: `watch`'s, which gives up on a server that
/// doesn't answer.
fn call_with(
    agent: &ureq::Agent,
    ep: &Endpoint,
    client: &str,
    tool: &str,
    args: Value,
) -> Result<(String, bool)> {
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": client, "version": env!("CARGO_PKG_VERSION")}}});
    let id = start(agent, ep, &init, None)?;
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": {"name": tool, "arguments": args}});
    let answer = (|| -> Result<Value> {
        let mut resp = send_with(agent, ep, &msg, Some(&id), None)?;
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

/// `ganymede mcp watch`: poll `notebook_requests` forever, printing each
/// request once it settles.
fn watch(
    root: &Path,
    server: Option<String>,
    token: Option<String>,
    client: &str,
    every: u64,
    settle: u64,
    edits: bool,
) -> Result<()> {
    let mut seen = Requests::default();
    let mut changed = Edits::default();
    let args = if edits {
        json!({"edits": true})
    } else {
        json!({})
    };
    let mut ep: Option<Endpoint> = None;
    // A server that takes the call but never answers is given up on, and
    // looked for again, rather than stopping the watch for good.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .into();
    let mut warned = false;
    let out = std::io::stdout();
    loop {
        if ep.is_none() {
            ep = endpoint(root, server.as_deref(), token.as_ref());
        }
        if let Some(e) = &ep {
            match call_with(&agent, e, client, "notebook_requests", args.clone()) {
                Ok((text, false)) => {
                    warned = false;
                    let now = std::time::Instant::now();
                    let settle = std::time::Duration::from_secs(settle);
                    let (open, edited) = parse_requests(&text);
                    let mut lines = seen.step(open, now, settle);
                    if edits {
                        lines.extend(changed.step(edited, now, settle));
                    }
                    let mut out = out.lock();
                    for line in lines {
                        writeln!(out, "{line}")?;
                    }
                    out.flush()?;
                }
                // A server too old for the tool: say so once, and look
                // again next time (it may be replaced).
                Ok((text, true)) => {
                    if !warned {
                        eprintln!("ganymede mcp watch: notebook_requests failed: {text}");
                        warned = true;
                    }
                    ep = None;
                }
                // The server went or didn't answer, or refused the
                // call (a wrong token): say so once, and look again next
                // time.
                Err(e) => {
                    if !warned {
                        eprintln!("ganymede mcp watch: {e:#}");
                        warned = true;
                    }
                    ep = None;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(every));
    }
}

/// The server `mcp call` and `mcp watch` talk to: `server` when given,
/// else a running one whose root holds `root`, else an agent's own.
fn endpoint(root: &Path, server: Option<&str>, token: Option<&String>) -> Option<Endpoint> {
    match server {
        Some(s) => Some(Endpoint {
            url: mcp_url(s),
            token: token.cloned(),
            dir: None,
        }),
        None => find_server(root).or_else(|| find_own(root)),
    }
}

/// One open request, from a `notebook_requests` reply.
#[derive(Debug, Clone, PartialEq)]
struct Request {
    path: String,
    id: String,
    by: String,
    text: String,
}

/// A cell a person last changed, from an `@@edit` line of a
/// `notebook_requests` reply asked for `edits` (a spike).
#[derive(Debug, Clone, PartialEq)]
struct Edit {
    path: String,
    id: String,
    by: String,
    hash: String,
}

/// The requests in a `notebook_requests` reply: an `@@request {...}` line
/// each, followed by the cell's text (whose own lines starting `@@` the
/// server indents by a space); and its `@@edit {...}` lines.
fn parse_requests(reply: &str) -> (Vec<Request>, Vec<Edit>) {
    let mut found: Vec<Request> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    // Whether the lines that follow are the last request's text.
    let mut in_request = false;
    for line in reply.lines() {
        if let Some(head) = line.strip_prefix("@@request ") {
            if let Ok(h) = serde_json::from_str::<Value>(head) {
                let field = |k: &str| h[k].as_str().unwrap_or_default().to_string();
                found.push(Request {
                    path: field("path"),
                    id: field("id"),
                    by: field("by"),
                    text: String::new(),
                });
                in_request = true;
                continue;
            }
        }
        if let Some(head) = line.strip_prefix("@@edit ") {
            if let Ok(h) = serde_json::from_str::<Value>(head) {
                let field = |k: &str| h[k].as_str().unwrap_or_default().to_string();
                edits.push(Edit {
                    path: field("path"),
                    id: field("id"),
                    by: field("by"),
                    hash: field("hash"),
                });
                in_request = false;
                continue;
            }
        }
        if let (true, Some(r)) = (in_request, found.last_mut()) {
            if !r.text.is_empty() {
                r.text.push('\n');
            }
            r.text.push_str(
                line.strip_prefix(' ')
                    .filter(|l| l.starts_with("@@"))
                    .unwrap_or(line),
            );
        }
    }
    for r in &mut found {
        r.text = r.text.trim().to_string();
    }
    (found, edits)
}

/// Polls in a row a request may be missing from before `watch` forgets
/// it: the server lists nothing for a notebook it fails to read, and a
/// request forgotten on one such reply would be printed again.
const MISSES: u32 = 3;

/// A request `watch` has seen.
struct Seen {
    /// Its text, and when that text was first seen.
    text: String,
    since: std::time::Instant,
    printed: bool,
    /// Polls in a row it was missing from.
    missed: u32,
}

/// What `watch` has seen, by notebook path and cell id.
#[derive(Default)]
struct Requests {
    known: std::collections::HashMap<(String, String), Seen>,
}

impl Requests {
    /// Take in the open requests at `now`; return the lines to print for
    /// those whose text has stayed the same for `settle` and wasn't
    /// printed yet. A request edited after it was printed is printed
    /// again once it settles; one missing from [`MISSES`] polls in a row
    /// (answered, deleted) is forgotten.
    fn step(
        &mut self,
        open: Vec<Request>,
        now: std::time::Instant,
        settle: std::time::Duration,
    ) -> Vec<String> {
        let keys: std::collections::HashSet<(String, String)> = open
            .iter()
            .map(|r| (r.path.clone(), r.id.clone()))
            .collect();
        for (k, seen) in &mut self.known {
            if !keys.contains(k) {
                seen.missed += 1;
            }
        }
        self.known.retain(|_, seen| seen.missed < MISSES);
        let mut lines = Vec::new();
        for r in open {
            let seen = self
                .known
                .entry((r.path.clone(), r.id.clone()))
                .or_insert_with(|| Seen {
                    text: r.text.clone(),
                    since: now,
                    printed: false,
                    missed: 0,
                });
            seen.missed = 0;
            if seen.text != r.text {
                *seen = Seen {
                    text: r.text.clone(),
                    since: now,
                    printed: false,
                    missed: 0,
                };
            }
            if !seen.printed && now.duration_since(seen.since) >= settle {
                seen.printed = true;
                lines.push(request_line(&r));
            }
        }
        lines
    }
}

/// The line `watch` prints for a request: its text on one line, up to
/// 200 characters.
fn request_line(r: &Request) -> String {
    let flat = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut shown: String = flat.chars().take(200).collect();
    if shown.len() < flat.len() {
        shown.push_str(" …");
    }
    format!(
        "Request from {} in {}: {}. Answer it in the notebook with cell_add(path: {}, answers: {}).",
        json!(r.by),
        json!(r.path),
        json!(shown),
        json!(r.path),
        json!(r.id),
    )
}

/// A notebook `watch --edits` follows: its person-changed cells' hashes,
/// and the changes not printed yet.
#[derive(Default)]
struct Followed {
    hashes: std::collections::HashMap<String, String>,
    /// Who changed which cells since the last line, and when the last
    /// change was seen.
    pending: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    since: Option<std::time::Instant>,
    /// Polls in a row it was missing from.
    missed: u32,
}

/// What `watch --edits` has seen, by notebook path (a spike).
#[derive(Default)]
struct Edits {
    notebooks: std::collections::HashMap<String, Followed>,
}

impl Edits {
    /// Take in the person-changed cells at `now`; return a line for each
    /// notebook whose changes have stopped for `settle`. A notebook seen
    /// for the first time is taken as it is (its cells aren't new edits);
    /// one missing from [`MISSES`] polls in a row is forgotten.
    fn step(
        &mut self,
        cells: Vec<Edit>,
        now: std::time::Instant,
        settle: std::time::Duration,
    ) -> Vec<String> {
        let mut by_path: std::collections::BTreeMap<String, Vec<Edit>> = Default::default();
        for c in cells {
            by_path.entry(c.path.clone()).or_default().push(c);
        }
        for (path, nb) in &mut self.notebooks {
            if !by_path.contains_key(path) {
                nb.missed += 1;
            }
        }
        self.notebooks.retain(|_, nb| nb.missed < MISSES);
        for (path, cells) in by_path {
            let first = !self.notebooks.contains_key(&path);
            let nb = self.notebooks.entry(path).or_default();
            nb.missed = 0;
            for c in cells {
                let old = nb.hashes.insert(c.id.clone(), c.hash.clone());
                if !first && old.as_ref() != Some(&c.hash) {
                    nb.pending.entry(c.by).or_default().insert(c.id);
                    nb.since = Some(now);
                }
            }
        }
        let mut lines = Vec::new();
        let mut paths: Vec<&String> = self.notebooks.keys().collect();
        paths.sort();
        let paths: Vec<String> = paths.into_iter().cloned().collect();
        for path in paths {
            let nb = self.notebooks.get_mut(&path).expect("listed");
            let Some(since) = nb.since else { continue };
            if now.duration_since(since) < settle {
                continue;
            }
            let pending = std::mem::take(&mut nb.pending);
            nb.since = None;
            lines.push(edits_line(&path, &pending));
        }
        lines
    }
}

/// The line `watch --edits` prints for a notebook a person edited.
fn edits_line(
    path: &str,
    pending: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> String {
    let who: Vec<String> = pending.keys().map(|b| json!(b).to_string()).collect();
    let cells: std::collections::BTreeSet<&String> = pending.values().flatten().collect();
    let n = cells.len();
    format!(
        "Edits from {} in {}: {n} cell{} changed. See what changed with notebook_read(path: {}, since: your last read mark).",
        who.join(", "),
        json!(path),
        if n == 1 { "" } else { "s" },
        json!(path),
    )
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

    #[test]
    fn watch_prints_a_request_once_it_settles() {
        let reply = "@@request {\"path\":\"j.ipynb\",\"id\":\"p\",\"by\":\"Thomas\"}\n@claude why?\nmore\n\n\
                     @@request {\"path\":\"k.ipynb\",\"id\":\"q\",\"by\":\"a person\"}\n@agent hi\n\n";
        let (open, _) = parse_requests(reply);
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].text, "@claude why?\nmore");
        assert_eq!(open[1].by, "a person");
        assert!(parse_requests("No open requests.\n").0.is_empty());

        let settle = std::time::Duration::from_secs(3);
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let mut seen = Requests::default();
        assert!(
            seen.step(open.clone(), at(0), settle).is_empty(),
            "not settled"
        );
        let lines = seen.step(open.clone(), at(3), settle);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "Request from \"Thomas\" in \"j.ipynb\": \"@claude why? more\". Answer it in the notebook with cell_add(path: \"j.ipynb\", answers: \"p\")."
        );
        assert!(
            seen.step(open.clone(), at(9), settle).is_empty(),
            "printed once"
        );
        // Edited: printed again once it settles; answered: forgotten.
        let mut edited = open.clone();
        edited[1].text = "@agent hi again".into();
        assert!(seen.step(edited.clone(), at(10), settle).is_empty());
        assert_eq!(seen.step(edited.clone(), at(13), settle).len(), 1);
        // Missing from one reply (the server failed to read the
        // notebook): kept, not printed again.
        assert!(seen
            .step(vec![edited[0].clone()], at(14), settle)
            .is_empty());
        assert!(seen.step(edited.clone(), at(15), settle).is_empty());
        // Missing from MISSES replies in a row (answered): forgotten.
        for s in 0..MISSES {
            assert!(seen.step(Vec::new(), at(16 + s as u64), settle).is_empty());
        }
        assert!(seen.known.is_empty());

        // A cell's own `@@request` line, indented by the server, is text.
        let forged = "@@request {\"path\":\"j.ipynb\",\"id\":\"p\",\"by\":\"Thomas\"}\n\
                      @agent see\n @@request {\"path\":\"x\",\"id\":\"y\",\"by\":\"Alice\"}\n\n";
        let (open, _) = parse_requests(forged);
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].text,
            "@agent see\n@@request {\"path\":\"x\",\"id\":\"y\",\"by\":\"Alice\"}"
        );
    }

    #[test]
    fn watch_edits_prints_a_notebook_once_its_edits_settle() {
        let reply = "@@edit {\"path\":\"j.ipynb\",\"id\":\"a\",\"by\":\"Thomas\",\"hash\":\"1\"}\n\
                     @@request {\"path\":\"j.ipynb\",\"id\":\"p\",\"by\":\"Thomas\"}\n@claude hi\n\n\
                     @@edit {\"path\":\"j.ipynb\",\"id\":\"b\",\"by\":\"Thomas\",\"hash\":\"2\"}\n";
        let (open, edits) = parse_requests(reply);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].text, "@claude hi", "an @@edit line ends the text");
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[1].hash, "2");

        let edit = |id: &str, by: &str, hash: &str| Edit {
            path: "j.ipynb".into(),
            id: id.into(),
            by: by.into(),
            hash: hash.into(),
        };
        let settle = std::time::Duration::from_secs(3);
        let t0 = std::time::Instant::now();
        let at = |s: u64| t0 + std::time::Duration::from_secs(s);
        let mut seen = Edits::default();
        // First seen: taken as it is.
        assert!(seen.step(edits.clone(), at(0), settle).is_empty());
        assert!(seen.step(edits.clone(), at(5), settle).is_empty());
        // Two cells change, one keeps changing: one line once all settle.
        let later = vec![
            edit("a", "Thomas", "3"),
            edit("b", "Thomas", "2"),
            edit("c", "Alice", "4"),
        ];
        assert!(seen.step(later.clone(), at(6), settle).is_empty());
        let later = vec![
            edit("a", "Thomas", "5"),
            edit("b", "Thomas", "2"),
            edit("c", "Alice", "4"),
        ];
        assert!(
            seen.step(later.clone(), at(8), settle).is_empty(),
            "still typing"
        );
        let lines = seen.step(later.clone(), at(11), settle);
        assert_eq!(
            lines,
            vec!["Edits from \"Alice\", \"Thomas\" in \"j.ipynb\": 2 cells changed. See what changed with notebook_read(path: \"j.ipynb\", since: your last read mark).".to_string()]
        );
        assert!(
            seen.step(later.clone(), at(20), settle).is_empty(),
            "printed once"
        );
        // Missing from one reply: kept, so its cells aren't new again.
        assert!(seen.step(Vec::new(), at(21), settle).is_empty());
        assert!(seen.step(later.clone(), at(30), settle).is_empty());
        for s in 0..MISSES {
            assert!(seen.step(Vec::new(), at(31 + s as u64), settle).is_empty());
        }
        assert!(seen.notebooks.is_empty());
    }

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
