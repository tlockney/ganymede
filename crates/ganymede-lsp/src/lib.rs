//! Language servers for `jupyterlab-lsp`, as the `jupyter-lsp` server
//! extension provides them: `GET /lsp/status` lists the configured
//! servers, and `/lsp/ws/{name}` relays JSON-RPC between websocket clients
//! and the server's stdio.
//!
//! Servers come only from configuration, in `jupyter-lsp`'s spec format
//! (`argv`, `languages`, `mime_types`, ...). There is no autodetection and
//! no built-in spec, so the host still knows no language.
//!
//! Each configured server runs as one process shared by every client of
//! it: started when the first client connects, ended when the last one
//! leaves. What the server sends goes to every client, and every client's
//! messages go to the server unchanged, except that notebook documents are
//! also written to disk for servers that read files (see [`shadow`]).
//!
//! On iOS and wasm an app cannot start processes, so
//! [`LspManager::supported`] is false and [`LspManager::connect`] fails.

// Without processes, the relay's process side is never reached.
#![cfg_attr(
    any(
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "visionos",
        target_family = "wasm"
    ),
    allow(dead_code)
)]

pub mod framing;
pub mod shadow;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The spec format version `jupyter-lsp` 2.x reports.
pub const SPEC_VERSION: u32 = 2;

/// Where virtual documents are written, under the contents root, as
/// `jupyter-lsp`'s default `virtual_documents_dir`.
pub const VIRTUAL_DOCUMENTS_DIR: &str = ".virtual_documents";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no language server named {0:?}")]
    NotFound(String),
    #[error("language servers are not available on this platform")]
    Unsupported,
    #[error("could not start language server {0:?}: {1}")]
    Spawn(String, String),
}

impl Error {
    /// The HTTP status to answer with.
    pub fn status(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::Unsupported | Error::Spawn(..) => 500,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// One language server, in `jupyter-lsp`'s spec format. Fields this crate
/// doesn't use (`urls`, `install`, `config_schema`,
/// `workspace_configuration`, ...) are kept and reported to clients as
/// they are.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct LanguageServerSpec {
    /// The command, which must speak LSP on stdin and stdout. Empty turns
    /// the server off, as in `jupyter-lsp`.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Languages it serves, matched against a document's language.
    #[serde(default)]
    pub languages: Vec<String>,
    /// MIME types it serves, matched against an editor's MIME type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mime_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Added to the server's environment. `$VAR` and `${VAR}` in a value
    /// are replaced from the server's own environment; `$$` is a `$`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Write notebooks' virtual documents to disk for this server. True
    /// by default, as in `jupyter-lsp`; servers that keep documents in
    /// memory can turn it off.
    #[serde(default = "yes")]
    pub requires_documents_on_disk: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn yes() -> bool {
    true
}

impl LanguageServerSpec {
    /// The spec as `/lsp/status` reports it: with the command and
    /// environment blanked, as `jupyter-lsp` does, so the page learns
    /// nothing about the machine.
    pub fn censored(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        if let Value::Object(m) = &mut v {
            m.insert("version".into(), SPEC_VERSION.into());
            m.insert("argv".into(), json!([""]));
            if m.contains_key("debug_argv") {
                m.insert("debug_argv".into(), json!([""]));
            }
            if m.contains_key("env") {
                m.insert("env".into(), json!({}));
            }
        }
        v
    }
}

#[derive(Debug, Clone, Default)]
pub struct LspConfig {
    /// Keyed by the name used in `/lsp/ws/{name}`.
    pub servers: BTreeMap<String, LanguageServerSpec>,
    /// The contents root: servers start here, and virtual documents are
    /// written under it.
    pub root_dir: PathBuf,
}

/// A `file://` URI for a path, as the lab page config's `rootUri`.
pub fn file_uri(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    }
}

/// `virtualDocumentsUri` for the page config.
pub fn virtual_documents_uri(root_dir: &Path) -> String {
    file_uri(&root_dir.join(VIRTUAL_DOCUMENTS_DIR))
}

pub struct LspManager {
    sessions: BTreeMap<String, Arc<Session>>,
    shadow: Arc<shadow::Shadow>,
    root_dir: PathBuf,
}

impl LspManager {
    /// Specs with an empty `argv` are left out.
    pub fn new(config: LspConfig) -> Self {
        let sessions = config
            .servers
            .into_iter()
            .filter(|(name, spec)| {
                if spec.argv.is_empty() {
                    tracing::info!(server = %name, "language server has no argv; skipped");
                }
                !spec.argv.is_empty()
            })
            .map(|(name, spec)| {
                let s = Arc::new(Session {
                    name: name.clone(),
                    spec,
                    state: Mutex::new(State::default()),
                });
                (name, s)
            })
            .collect();
        let dir = config.root_dir.join(VIRTUAL_DOCUMENTS_DIR);
        let uri = virtual_documents_uri(&config.root_dir);
        LspManager {
            sessions,
            shadow: Arc::new(shadow::Shadow::new(dir, uri)),
            root_dir: config.root_dir,
        }
    }

    /// Whether this platform can start language servers at all.
    pub fn supported() -> bool {
        process::SUPPORTED
    }

    /// Whether any server is configured.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// `GET /lsp/status`: `{"version", "sessions", "specs"}`.
    pub fn status(&self) -> Value {
        let sessions: Map<String, Value> = self
            .sessions
            .iter()
            .map(|(name, s)| (name.clone(), s.model()))
            .collect();
        let specs: Map<String, Value> = self
            .sessions
            .iter()
            .map(|(name, s)| (name.clone(), s.spec.censored()))
            .collect();
        json!({"version": SPEC_VERSION, "sessions": sessions, "specs": specs})
    }

    /// Attaches a client to a server, starting the server if no client
    /// has it open.
    pub fn connect(&self, name: &str) -> Result<LspClient> {
        let session = self
            .sessions
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(name.into()))?;
        let mut st = session.state.lock().unwrap();
        if st.running.is_none() {
            st.generation += 1;
            let running = process::start(&session, st.generation, &self.root_dir)?;
            st.running = Some(running);
            st.status = Status::Started;
            tracing::info!(server = %session.name, argv = ?session.spec.argv, "language server started");
        }
        let (tx, rx) = mpsc::unbounded_channel();
        st.next_client += 1;
        let id = st.next_client;
        st.clients.insert(id, tx);
        let to_server = st.running.as_ref().expect("started").to_server.clone();
        drop(st);
        let shadow = session
            .spec
            .requires_documents_on_disk
            .then(|| self.shadow.clone());
        Ok(LspClient {
            session,
            id,
            rx,
            to_server,
            shadow,
        })
    }

    /// Ends every server, as the server stops.
    pub fn shutdown_all(&self) {
        for s in self.sessions.values() {
            s.stop();
        }
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Status {
    #[default]
    NotStarted,
    Started,
    Stopped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::NotStarted => "not_started",
            Status::Started => "started",
            Status::Stopped => "stopped",
        }
    }
}

struct Session {
    name: String,
    spec: LanguageServerSpec,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    status: Status,
    /// Bumped per process, so a process that ended late can't clear its
    /// successor.
    generation: u64,
    running: Option<process::Running>,
    clients: BTreeMap<u64, mpsc::UnboundedSender<Arc<str>>>,
    next_client: u64,
    last_handler_message_at: Option<String>,
    last_server_message_at: Option<String>,
}

impl Session {
    fn model(&self) -> Value {
        let st = self.state.lock().unwrap();
        json!({
            "handler_count": st.clients.len(),
            "status": st.status.as_str(),
            "last_handler_message_at": st.last_handler_message_at,
            "last_server_message_at": st.last_server_message_at,
            "spec": self.spec.censored(),
        })
    }

    /// A message from the server, to every client.
    fn broadcast(&self, message: String) {
        let message: Arc<str> = message.into();
        let mut st = self.state.lock().unwrap();
        st.last_server_message_at = Some(now_iso());
        st.clients.retain(|_, tx| tx.send(message.clone()).is_ok());
    }

    /// The process `generation` ended: its clients are closed, and the
    /// next client starts a new one.
    fn ended(&self, generation: u64) {
        let mut st = self.state.lock().unwrap();
        if st.generation != generation || st.running.is_none() {
            return;
        }
        st.running = None;
        st.status = Status::Stopped;
        st.clients.clear();
        tracing::info!(server = %self.name, "language server exited");
    }

    fn stop(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(r) = st.running.take() {
            r.kill();
            st.status = Status::Stopped;
            tracing::info!(server = %self.name, "language server stopped");
        }
        st.clients.clear();
    }

    fn disconnect(&self, id: u64) {
        let mut st = self.state.lock().unwrap();
        st.clients.remove(&id);
        if st.clients.is_empty() {
            if let Some(r) = st.running.take() {
                r.kill();
                st.status = Status::Stopped;
                tracing::info!(server = %self.name, "language server stopped: no clients");
            }
        }
    }
}

/// One websocket's view of a server.
pub struct LspClient {
    session: Arc<Session>,
    id: u64,
    rx: mpsc::UnboundedReceiver<Arc<str>>,
    to_server: mpsc::UnboundedSender<String>,
    shadow: Option<Arc<shadow::Shadow>>,
}

impl LspClient {
    pub fn server(&self) -> &str {
        &self.session.name
    }

    /// Sends a JSON-RPC message to the server, after writing the document
    /// it carries to disk if it is a virtual document.
    pub async fn send(&self, message: String) {
        self.session.state.lock().unwrap().last_handler_message_at = Some(now_iso());
        if let Some(shadow) = &self.shadow {
            if let Ok(v) = serde_json::from_str::<Value>(&message) {
                if let Err(e) = shadow.apply(&v).await {
                    tracing::warn!(server = %self.session.name, "virtual document not written: {e}");
                }
            }
        }
        let _ = self.to_server.send(message);
    }

    /// The next message from the server; `None` once the server has
    /// exited or was stopped.
    pub async fn recv(&mut self) -> Option<Arc<str>> {
        self.rx.recv().await
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.session.disconnect(self.id);
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

/// `$VAR`, `${VAR}`, and `$$` in `value`, from `lookup`, as Python's
/// `string.Template.safe_substitute`: unknown names are left as written.
fn expand_env(value: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(a) = after.strip_prefix('$') {
            out.push('$');
            rest = a;
            continue;
        }
        let ident_len = |s: &str| {
            s.char_indices()
                .find(|(j, c)| {
                    !(c.is_ascii_alphanumeric() || *c == '_') || (*j == 0 && c.is_ascii_digit())
                })
                .map_or(s.len(), |(j, _)| j)
        };
        let (name, consumed) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) if end > 0 && ident_len(&braced[..end]) == end => {
                    (&braced[..end], end + 2)
                }
                _ => ("", 0),
            }
        } else {
            let n = ident_len(after);
            (&after[..n], n)
        };
        match (name.is_empty(), lookup(name)) {
            (false, Some(v)) => out.push_str(&v),
            _ => {
                out.push('$');
                out.push_str(&after[..consumed]);
            }
        }
        rest = &after[consumed..];
    }
    out.push_str(rest);
    out
}

#[cfg(not(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_family = "wasm"
)))]
mod process {
    use super::{expand_env, framing, Error, Result, Session};
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot};

    pub const SUPPORTED: bool = true;

    pub struct Running {
        pub to_server: mpsc::UnboundedSender<String>,
        kill: oneshot::Sender<()>,
    }

    impl Running {
        pub fn kill(self) {
            let _ = self.kill.send(());
        }
    }

    pub fn start(session: &Arc<Session>, generation: u64, cwd: &Path) -> Result<Running> {
        let spec = &session.spec;
        let spawn_err = |e: String| Error::Spawn(session.name.clone(), e);
        let (prog, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| spawn_err("empty argv".into()))?;
        let mut cmd = tokio::process::Command::new(prog);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in &spec.env {
            cmd.env(k, expand_env(v, |name| std::env::var(name).ok()));
        }
        let mut child = cmd.spawn().map_err(|e| spawn_err(e.to_string()))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (to_server, mut from_clients) = mpsc::unbounded_channel::<String>();
        let (kill, killed) = oneshot::channel::<()>();

        tokio::spawn(async move {
            while let Some(msg) = from_clients.recv().await {
                if let Err(e) = framing::write_message(&mut stdin, &msg).await {
                    tracing::debug!("language server stdin closed: {e}");
                    break;
                }
            }
        });
        let reader_session = session.clone();
        tokio::spawn(async move {
            let mut r = tokio::io::BufReader::new(stdout);
            loop {
                match framing::read_message(&mut r).await {
                    Ok(Some(msg)) => reader_session.broadcast(msg),
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(server = %reader_session.name, "bad message from language server: {e}");
                        break;
                    }
                }
            }
            reader_session.ended(generation);
        });
        let name = session.name.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = killed => {
                    let _ = child.kill().await;
                }
                status = child.wait() => {
                    tracing::debug!(server = %name, ?status, "language server exited");
                }
            }
        });
        Ok(Running { to_server, kill })
    }
}

#[cfg(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_family = "wasm"
))]
mod process {
    use super::{Error, Result, Session};
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    pub const SUPPORTED: bool = false;

    pub struct Running {
        pub to_server: mpsc::UnboundedSender<String>,
    }

    impl Running {
        pub fn kill(self) {}
    }

    pub fn start(_: &Arc<Session>, _: u64, _: &Path) -> Result<Running> {
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion_is_safe_substitute() {
        let env = |n: &str| match n {
            "HOME" => Some("/home/u".to_string()),
            "A_1" => Some("x".to_string()),
            _ => None,
        };
        assert_eq!(expand_env("$HOME/bin", env), "/home/u/bin");
        assert_eq!(expand_env("${HOME}x:${A_1}", env), "/home/ux:x");
        assert_eq!(
            expand_env("$$HOME $NOPE ${NOPE} $ ${", env),
            "$HOME $NOPE ${NOPE} $ ${"
        );
        assert_eq!(expand_env("plain", env), "plain");
    }

    #[test]
    fn spec_from_json_and_censored() {
        let spec: LanguageServerSpec = serde_json::from_value(json!({
            "argv": ["/opt/ls", "--stdio"],
            "languages": ["python"],
            "env": {"SECRET": "1"},
            "debug_argv": ["/opt/ls", "-v"],
            "urls": {"home": "https://example.org"},
        }))
        .unwrap();
        assert!(spec.requires_documents_on_disk);
        assert_eq!(spec.extra["urls"]["home"], "https://example.org");
        let c = spec.censored();
        assert_eq!(c["argv"], json!([""]));
        assert_eq!(c["debug_argv"], json!([""]));
        assert_eq!(c["env"], json!({}));
        assert_eq!(c["version"], 2);
        assert_eq!(c["languages"], json!(["python"]));
        assert_eq!(c["urls"]["home"], "https://example.org");
        assert_eq!(c["requires_documents_on_disk"], true);
    }

    #[test]
    fn status_lists_configured_servers() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "on".to_string(),
            LanguageServerSpec {
                argv: vec!["ls".into()],
                languages: vec!["r".into()],
                ..Default::default()
            },
        );
        servers.insert("off".to_string(), LanguageServerSpec::default());
        let m = LspManager::new(LspConfig {
            servers,
            root_dir: "/srv/nb".into(),
        });
        let s = m.status();
        assert_eq!(s["version"], 2);
        assert_eq!(s["sessions"].as_object().unwrap().len(), 1);
        assert_eq!(s["sessions"]["on"]["status"], "not_started");
        assert_eq!(s["sessions"]["on"]["handler_count"], 0);
        assert_eq!(s["sessions"]["on"]["last_server_message_at"], Value::Null);
        assert_eq!(s["specs"]["on"]["languages"], json!(["r"]));
        assert!(matches!(m.connect("off"), Err(Error::NotFound(_))));
        assert_eq!(
            virtual_documents_uri(Path::new("/srv/nb")),
            "file:///srv/nb/.virtual_documents"
        );
    }
}
