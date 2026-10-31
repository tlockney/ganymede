//! Terminals for the Jupyter terminals API (`/api/terminals`): the user's
//! shell in a pseudo-terminal, which any number of clients can attach to.
//!
//! The shell is whatever the user's login shell is (or `terminal_shell`),
//! so the host still knows no language. Each terminal keeps the tail of its
//! output, sent to a client when it attaches, as terminado does.
//!
//! On iOS and wasm an app cannot start processes, so [`TerminalManager::supported`]
//! is false and [`TerminalManager::create`] fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::broadcast;

/// How much output a terminal keeps for clients that attach later.
const HISTORY_BYTES: usize = 128 * 1024;
/// Output chunks queued per client before a slow one starts losing them.
const CLIENT_QUEUE: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Terminal not found: {0}")]
    NotFound(String),
    #[error("A terminal with name '{0}' already exists.")]
    Exists(String),
    #[error("Invalid terminal name: {0:?}")]
    BadName(String),
    #[error("terminals are not available on this platform")]
    Unsupported,
    #[error("could not start the terminal: {0}")]
    Spawn(String),
}

impl Error {
    /// The HTTP status `jupyter_server` answers with.
    pub fn status(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::Exists(_) => 409,
            Error::BadName(_) => 400,
            Error::Unsupported | Error::Spawn(_) => 500,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `{"name": ..., "last_activity": ...}`, as `jupyter_server` lists terminals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalModel {
    pub name: String,
    /// ISO 8601, UTC.
    pub last_activity: String,
}

#[derive(Debug, Clone, Default)]
pub struct TerminalConfig {
    /// The command each terminal runs. Empty: the user's `$SHELL` as a
    /// login shell (`/bin/sh` without one), or `powershell.exe` on Windows.
    pub shell: Vec<String>,
    /// Where terminals start, and what a relative `cwd` is resolved
    /// against: the server's root dir.
    pub root_dir: PathBuf,
}

/// What a client receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Stdout(String),
    /// The shell exited or the terminal was deleted. Nothing follows.
    Exited,
}

pub struct TerminalManager {
    inner: Arc<Inner>,
}

struct Inner {
    config: TerminalConfig,
    terminals: Mutex<BTreeMap<String, Arc<Terminal>>>,
}

impl TerminalManager {
    pub fn new(config: TerminalConfig) -> Self {
        TerminalManager {
            inner: Arc::new(Inner {
                config,
                terminals: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Whether this platform can start terminals at all.
    pub fn supported() -> bool {
        pty::SUPPORTED
    }

    /// The command a new terminal runs.
    pub fn shell(&self) -> Vec<String> {
        if !self.inner.config.shell.is_empty() {
            return self.inner.config.shell.clone();
        }
        default_shell(std::env::var("SHELL").ok())
    }

    pub fn list(&self) -> Vec<TerminalModel> {
        let terms = self.inner.terminals.lock().unwrap();
        terms.values().map(|t| t.model()).collect()
    }

    pub fn get(&self, name: &str) -> Result<TerminalModel> {
        self.find(name).map(|t| t.model())
    }

    /// Starts a terminal. Unnamed ones get the lowest free number from 1,
    /// as terminado names them. `cwd` is used if it exists, absolute or
    /// relative to the root dir; otherwise the terminal starts in the root
    /// dir.
    pub fn create(&self, name: Option<String>, cwd: Option<&str>) -> Result<TerminalModel> {
        if !Self::supported() {
            return Err(Error::Unsupported);
        }
        let cwd = resolve_cwd(&self.inner.config.root_dir, cwd);
        let shell = self.shell();
        // Hold the map while spawning so two requests can't take one name.
        let mut terms = self.inner.terminals.lock().unwrap();
        let name = match name.filter(|n| !n.is_empty()) {
            Some(n) => {
                if n.contains('/') || n.chars().any(char::is_control) {
                    return Err(Error::BadName(n));
                }
                if terms.contains_key(&n) {
                    return Err(Error::Exists(n));
                }
                n
            }
            None => (1..)
                .map(|i: u64| i.to_string())
                .find(|n| !terms.contains_key(n))
                .expect("a free number"),
        };
        let process = pty::spawn(&shell, &cwd, &self.inner.config.root_dir)?;
        let term = Terminal::start(name.clone(), process, Arc::downgrade(&self.inner));
        tracing::info!(terminal = %name, cmd = ?shell, cwd = %cwd.display(), "terminal started");
        let model = term.model();
        terms.insert(name, term);
        Ok(model)
    }

    /// Ends the terminal's shell and forgets it.
    pub fn delete(&self, name: &str) -> Result<()> {
        let term = self
            .inner
            .terminals
            .lock()
            .unwrap()
            .remove(name)
            .ok_or_else(|| Error::NotFound(name.into()))?;
        term.kill();
        Ok(())
    }

    /// Attaches a client. It gets the kept output first, then what follows.
    pub fn connect(&self, name: &str) -> Result<TerminalClient> {
        let term = self.find(name)?;
        // Subscribe and copy the history under one lock, so no chunk is
        // both in the history and queued, or in neither.
        let (history, rx) = {
            let h = term.history.lock().unwrap();
            (h.text.clone(), term.tx.subscribe())
        };
        Ok(TerminalClient {
            term,
            rx,
            history: Some(history),
        })
    }

    /// Ends every terminal, as the server stops.
    pub fn shutdown_all(&self) {
        let terms = std::mem::take(&mut *self.inner.terminals.lock().unwrap());
        for t in terms.values() {
            t.kill();
        }
    }

    fn find(&self, name: &str) -> Result<Arc<Terminal>> {
        self.inner
            .terminals
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::NotFound(name.into()))
    }
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

/// `$SHELL -l`, as `jupyter_server` starts a login shell when it isn't
/// itself run from a terminal, so the profile sets `PATH` and the like.
fn default_shell(env_shell: Option<String>) -> Vec<String> {
    if cfg!(windows) {
        return vec!["powershell.exe".into()];
    }
    let shell = env_shell
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    vec![shell, "-l".into()]
}

fn resolve_cwd(root: &Path, cwd: Option<&str>) -> PathBuf {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return root.to_path_buf();
    };
    let p = Path::new(cwd);
    if p.is_absolute() && p.is_dir() {
        return p.to_path_buf();
    }
    let joined = root.join(cwd.trim_start_matches('/'));
    if joined.is_dir() {
        joined
    } else {
        root.to_path_buf()
    }
}

/// One attached client: input goes to the shell, output comes back.
pub struct TerminalClient {
    term: Arc<Terminal>,
    rx: broadcast::Receiver<Output>,
    history: Option<String>,
}

impl TerminalClient {
    pub fn name(&self) -> &str {
        &self.term.name
    }

    /// The output kept from before this client attached; `None` after the
    /// first call.
    pub fn take_history(&mut self) -> Option<String> {
        self.history.take().filter(|h| !h.is_empty())
    }

    /// Sends keystrokes to the shell. Never blocks: a writer thread feeds
    /// the pseudo-terminal.
    pub fn write(&self, data: &str) {
        self.term.touch();
        let _ = self.term.input.send(data.as_bytes().to_vec());
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        if let Err(e) = self.term.process.resize(rows, cols) {
            tracing::debug!(terminal = %self.term.name, "resize failed: {e}");
        }
    }

    /// The next output, or `None` once the terminal has ended and
    /// [`Output::Exited`] was delivered. A client that falls more than
    /// 1024 chunks behind skips the ones it missed.
    pub async fn recv(&mut self) -> Option<Output> {
        loop {
            match self.rx.recv().await {
                Ok(o) => return Some(o),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(terminal = %self.term.name, skipped = n, "slow terminal client");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

struct Terminal {
    name: String,
    process: pty::Process,
    input: std::sync::mpsc::Sender<Vec<u8>>,
    tx: broadcast::Sender<Output>,
    history: Mutex<History>,
    last_activity: Mutex<String>,
}

#[derive(Default)]
struct History {
    text: String,
    ended: bool,
}

impl Terminal {
    fn start(name: String, process: pty::Process, manager: Weak<Inner>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(CLIENT_QUEUE);
        let (input, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let reader = process.take_reader();
        let mut writer = process.take_writer();
        let term = Arc::new(Terminal {
            name,
            process,
            input,
            tx,
            history: Mutex::new(History::default()),
            last_activity: Mutex::new(now_iso()),
        });
        // Input: blocking writes off the async workers.
        std::thread::Builder::new()
            .name(format!("terminal-{}-in", term.name))
            .spawn(move || {
                use std::io::Write;
                while let Ok(bytes) = input_rx.recv() {
                    if writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn terminal writer");
        // Output, until the pseudo-terminal closes; then the exit.
        let weak = Arc::downgrade(&term);
        std::thread::Builder::new()
            .name(format!("terminal-{}-out", term.name))
            .spawn(move || read_loop(reader, weak, manager))
            .expect("spawn terminal reader");
        term
    }

    fn model(&self) -> TerminalModel {
        TerminalModel {
            name: self.name.clone(),
            last_activity: self.last_activity.lock().unwrap().clone(),
        }
    }

    fn touch(&self) {
        *self.last_activity.lock().unwrap() = now_iso();
    }

    fn emit(&self, text: String) {
        self.touch();
        let mut h = self.history.lock().unwrap();
        if h.ended {
            return;
        }
        h.text.push_str(&text);
        if h.text.len() > HISTORY_BYTES {
            let mut cut = h.text.len() - HISTORY_BYTES;
            while !h.text.is_char_boundary(cut) {
                cut += 1;
            }
            h.text.drain(..cut);
        }
        let _ = self.tx.send(Output::Stdout(text));
    }

    fn end(&self) {
        let mut h = self.history.lock().unwrap();
        if !h.ended {
            h.ended = true;
            let _ = self.tx.send(Output::Exited);
        }
    }

    fn kill(&self) {
        self.process.kill();
        self.end();
    }
}

fn read_loop(
    mut reader: Box<dyn std::io::Read + Send>,
    term: Weak<Terminal>,
    manager: Weak<Inner>,
) {
    let mut buf = [0u8; 8192];
    let mut decoder = Utf8Decoder::default();
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let text = decoder.push(&buf[..n]);
        let Some(t) = term.upgrade() else { return };
        if !text.is_empty() {
            t.emit(text);
        }
    }
    let Some(t) = term.upgrade() else { return };
    let code = t.process.wait();
    tracing::info!(terminal = %t.name, ?code, "terminal exited");
    t.end();
    if let Some(m) = manager.upgrade() {
        let mut terms = m.terminals.lock().unwrap();
        if terms.get(&t.name).is_some_and(|x| Arc::ptr_eq(x, &t)) {
            terms.remove(&t.name);
        }
    }
}

/// Bytes to text, keeping a UTF-8 sequence split across reads for the next
/// one; invalid bytes become U+FFFD.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap());
                    match e.error_len() {
                        Some(len) => {
                            out.push('\u{FFFD}');
                            rest = &after[len..];
                        }
                        None => {
                            rest = after;
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[cfg(not(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_family = "wasm"
)))]
mod pty {
    use super::{Error, Result};
    use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
    use std::path::Path;
    use std::sync::Mutex;

    pub const SUPPORTED: bool = true;

    /// Separate locks, so a kill or resize never waits behind `wait`.
    pub struct Process {
        master: Mutex<Box<dyn MasterPty + Send>>,
        child: Mutex<Box<dyn Child + Send + Sync>>,
        killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
        reader: Mutex<Option<Box<dyn std::io::Read + Send>>>,
        writer: Mutex<Option<Box<dyn std::io::Write + Send>>>,
    }

    pub fn spawn(argv: &[String], cwd: &Path, root: &Path) -> Result<Process> {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| Error::Spawn("empty terminal command".into()))?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Spawn(e.to_string()))?;
        let mut cmd = CommandBuilder::new(prog);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("JUPYTER_SERVER_ROOT", root);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Spawn(format!("{prog}: {e}")))?;
        // The parent's copy of the slave would keep the terminal open
        // after the shell exits.
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Spawn(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Spawn(e.to_string()))?;
        let killer = child.clone_killer();
        Ok(Process {
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            killer: Mutex::new(killer),
            reader: Mutex::new(Some(reader)),
            writer: Mutex::new(Some(writer)),
        })
    }

    impl Process {
        pub fn take_reader(&self) -> Box<dyn std::io::Read + Send> {
            self.reader
                .lock()
                .unwrap()
                .take()
                .expect("reader taken once")
        }

        pub fn take_writer(&self) -> Box<dyn std::io::Write + Send> {
            self.writer
                .lock()
                .unwrap()
                .take()
                .expect("writer taken once")
        }

        pub fn resize(&self, rows: u16, cols: u16) -> std::result::Result<(), String> {
            self.master
                .lock()
                .unwrap()
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| e.to_string())
        }

        pub fn kill(&self) {
            let _ = self.killer.lock().unwrap().kill();
        }

        /// The exit code, once the shell has exited.
        pub fn wait(&self) -> Option<u32> {
            self.child
                .lock()
                .unwrap()
                .wait()
                .ok()
                .map(|s| s.exit_code())
        }
    }
}

#[cfg(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
    target_family = "wasm"
))]
mod pty {
    use super::{Error, Result};
    use std::path::Path;

    pub const SUPPORTED: bool = false;

    pub enum Process {}

    pub fn spawn(_: &[String], _: &Path, _: &Path) -> Result<Process> {
        Err(Error::Unsupported)
    }

    impl Process {
        pub fn take_reader(&self) -> Box<dyn std::io::Read + Send> {
            match *self {}
        }
        pub fn take_writer(&self) -> Box<dyn std::io::Write + Send> {
            match *self {}
        }
        pub fn resize(&self, _: u16, _: u16) -> std::result::Result<(), String> {
            match *self {}
        }
        pub fn kill(&self) {
            match *self {}
        }
        pub fn wait(&self) -> Option<u32> {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_keeps_split_sequences_and_replaces_bad_bytes() {
        let mut d = Utf8Decoder::default();
        let snow = "☃".as_bytes();
        assert_eq!(d.push(&[b'a', snow[0]]), "a");
        assert_eq!(d.push(&snow[1..]), "☃");
        assert_eq!(d.push(&[0xff, b'b']), "\u{FFFD}b");
    }

    #[test]
    fn default_shell_is_a_login_shell() {
        if cfg!(windows) {
            assert_eq!(default_shell(None), ["powershell.exe"]);
        } else {
            assert_eq!(default_shell(Some("/bin/zsh".into())), ["/bin/zsh", "-l"]);
            assert_eq!(default_shell(None), ["/bin/sh", "-l"]);
            assert_eq!(default_shell(Some(String::new())), ["/bin/sh", "-l"]);
        }
    }

    #[test]
    fn cwd_resolves_against_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let r = root.path();
        assert_eq!(resolve_cwd(r, None), r);
        assert_eq!(resolve_cwd(r, Some("sub")), r.join("sub"));
        assert_eq!(resolve_cwd(r, Some("/sub")), r.join("sub"));
        assert_eq!(resolve_cwd(r, Some("missing")), r);
        let abs = r.join("sub");
        assert_eq!(resolve_cwd(r, abs.to_str()), abs);
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use std::time::Duration;

        fn manager(root: &Path) -> TerminalManager {
            TerminalManager::new(TerminalConfig {
                shell: vec!["/bin/sh".into()],
                root_dir: root.into(),
            })
        }

        async fn read_until(c: &mut TerminalClient, needle: &str) -> String {
            let mut seen = c.take_history().unwrap_or_default();
            tokio::time::timeout(Duration::from_secs(10), async {
                while !seen.contains(needle) {
                    match c.recv().await {
                        Some(Output::Stdout(s)) => seen.push_str(&s),
                        other => panic!("{other:?} before {needle:?} in {seen:?}"),
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} in {seen:?}"));
            seen
        }

        #[tokio::test]
        async fn names_echo_resize_and_delete() {
            let root = tempfile::tempdir().unwrap();
            let m = manager(root.path());
            assert_eq!(m.create(None, None).unwrap().name, "1");
            assert_eq!(m.create(None, None).unwrap().name, "2");
            assert!(matches!(
                m.create(Some("1".into()), None),
                Err(Error::Exists(_))
            ));
            assert!(matches!(
                m.create(Some("a/b".into()), None),
                Err(Error::BadName(_))
            ));
            assert_eq!(m.create(Some("work".into()), None).unwrap().name, "work");
            let names: Vec<_> = m.list().into_iter().map(|t| t.name).collect();
            assert_eq!(names, ["1", "2", "work"]);

            let mut c = m.connect("1").unwrap();
            c.write("echo $((6*7))\n");
            read_until(&mut c, "42").await;
            c.resize(40, 132);
            c.write("stty size\n");
            read_until(&mut c, "40 132").await;
            c.write("pwd\n");
            let root_name = root
                .path()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            read_until(&mut c, &root_name).await;

            // A second client sees the history, then live output.
            let mut late = m.connect("1").unwrap();
            assert!(late.take_history().unwrap().contains("42"));
            c.write("echo late-$((1+1))\n");
            read_until(&mut late, "late-2").await;

            m.delete("1").unwrap();
            assert!(matches!(m.get("1"), Err(Error::NotFound(_))));
            assert!(matches!(m.delete("1"), Err(Error::NotFound(_))));
            let end = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match c.recv().await {
                        Some(Output::Stdout(_)) => continue,
                        other => return other,
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(end, Some(Output::Exited));
            // The freed number is reused.
            assert_eq!(m.create(None, None).unwrap().name, "1");
        }

        #[tokio::test]
        async fn exiting_shell_ends_and_removes_the_terminal() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("sub")).unwrap();
            let m = manager(root.path());
            m.create(Some("t".into()), Some("sub")).unwrap();
            let mut c = m.connect("t").unwrap();
            c.write("pwd; exit\n");
            read_until(&mut c, "sub").await;
            let end = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match c.recv().await {
                        Some(Output::Stdout(_)) => continue,
                        other => return other,
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(end, Some(Output::Exited));
            for _ in 0..100 {
                if m.list().is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("terminal still listed: {:?}", m.list());
        }

        #[test]
        fn missing_shell_is_an_error() {
            let root = tempfile::tempdir().unwrap();
            let m = TerminalManager::new(TerminalConfig {
                shell: vec!["/no/such/shell".into()],
                root_dir: root.path().into(),
            });
            assert!(matches!(m.create(None, None), Err(Error::Spawn(_))));
            assert!(m.list().is_empty());
        }
    }
}
