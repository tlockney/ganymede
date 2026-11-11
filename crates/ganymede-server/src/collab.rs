//! `/api/collaboration`: document rooms over websockets, compatible with
//! `jupyter-collaboration` (`jupyter_server_ydoc` 3), so JupyterLab with its
//! prebuilt `@jupyter/docprovider-extension` edits notebooks through
//! Ganymede.
//!
//! - `PUT /api/collaboration/session/{path}` with `{"format", "type"}`
//!   answers `{"format", "type", "fileId", "sessionId"}` (201 the first
//!   time a path is seen, 200 after).
//! - `GET /api/collaboration/room/{format}:{type}:{fileId}?sessionId=` is
//!   the room's websocket (y-websocket sync and awareness, plus the raw
//!   `save` message). A room id without two colons, such as
//!   `JupyterLab:globalAwareness`, is a transient room: awareness only.
//!
//! Each room is a task that owns a [`Room`] and the file (#141): it loads
//! the notebook through [`Contents`] and autosaves a second after the last
//! edit, with an `If-Match` on the hash it last read or wrote, so it never
//! overwrites a file that changed since. It learns of changes made by other
//! programs from a [`Watcher`] on the file's directory, debounced by half a
//! second (at most five, for a file that never settles), and polls every
//! ten seconds besides; where there are no file
//! events, it polls every second. A change whose hash is the room's own
//! last save is ignored. Any other change is merged into the room (see
//! [`ganymede_documents::merge`]): a clean room just reloads; a room with
//! unsaved edits keeps them, takes the disk's changes to other cells, and
//! saves the merge. Where both changed the same cell, the disk wins that
//! cell. A room with no clients is saved and dropped after a minute. A
//! delete through the contents API closes the rooms on the deleted file.
//!
//! On one machine, a room owns its file while it is open: it holds a lock
//! in the runtime dir (see `room_lock.rs`). A room opened on a file
//! another server's room owns is read-only: it reloads the owner's saves,
//! refuses edits, never saves, and the contents API reports the file as
//! not `writable`. It takes the lock, and becomes writable, once the owner
//! closes its room or exits. Across machines (a synced folder) there is no
//! lock, and rooms merge through the file as above.
//!
//! File ids come from [`FileIds`]: kept in a database when the server has
//! one, so they survive restarts, and moved by renames through the
//! contents API, which also move open rooms.

use crate::routes::{origin_allowed, HttpError, LenientJson};
use crate::watch::{Watch, Watcher};
use crate::AppState;
// Frames queued for a client before it is dropped: the kernel bridge's
// limit. The kernel bridge drops output instead, but a room can't skip an
// update without the client's document going wrong, so the client goes
// and resyncs when it reconnects.
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ganymede_api::bridge::CLIENT_QUEUE_LIMIT;
use ganymede_contents::{ContentType, Contents, FileIds, GetOptions, IfMatch, SaveRequest};
use ganymede_documents::{ConnId, Room, SaveStatus, Target};
use ganymede_nbformat::Notebook;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long after the last edit a room saves.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// How often a room checks its file for changes made elsewhere, without
/// and with file events.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const WATCHED_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// How long file events must settle before a room looks at the file.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a room waits for events to settle at most: a file written
/// more often than [`DEBOUNCE`] is still looked at this often.
const MAX_DEBOUNCE: Duration = Duration::from_secs(5);
/// How long a room with no clients is kept, in case one reconnects.
const CLEANUP_DELAY: Duration = Duration::from_secs(60);
/// Failed saves in a row after which a room with no clients closes with
/// its edits unsaved: about five minutes of #353's backoff.
const GIVE_UP_AFTER: u32 = 10;
/// How often a room's websocket is pinged.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// A client that sends nothing, not even a pong, for this long is
/// dropped: its connection is half-open (a laptop asleep, a network gone).
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long one write to a client's websocket may take, plus a second
/// for every [`MIN_WRITE_RATE`] bytes in it, so a large room's first sync
/// over a slow link still gets through.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// The slowest link, in bytes a second, a large frame is given time for.
const MIN_WRITE_RATE: u64 = 64 * 1024;

/// Rooms and file ids for one server.
pub struct Collab {
    /// Changes every server run; clients holding another one are asked to
    /// reload, as `jupyter-collaboration` does after a restart.
    session_id: String,
    /// In an `Arc` so its calls, SQLite queries with a database, run on a
    /// blocking thread ([`Collab::with_ids`]).
    ids: Arc<FileIds>,
    /// The contents root on disk, for watching files; `None` when the
    /// contents are not a local directory, and rooms poll.
    root: Option<PathBuf>,
    watcher: Arc<Watcher>,
    rooms: Mutex<HashMap<String, mpsc::UnboundedSender<Cmd>>>,
    /// Bumped, under the rooms lock, by every rename or delete, so a path
    /// looked up without the lock can be checked before a room opens on it.
    moves: AtomicU64,
    /// Held for writing by a rename or delete through the contents API,
    /// from its disk change until the ids follow ([`Collab::begin_move`]),
    /// and for reading by a room opening across its path lookup and by
    /// [`Collab::index_existing`], so no room opens and no id is made for a
    /// path that is moving away or going (#522, #574).
    move_lock: tokio::sync::RwLock<()>,
    next_conn: AtomicU64,
    /// How long a room with no clients is kept: [`CLEANUP_DELAY`].
    cleanup_delay: Duration,
    /// How often sockets are pinged, and how long one may stay silent:
    /// [`PING_INTERVAL`] and [`IDLE_TIMEOUT`].
    heartbeat: (Duration, Duration),
    /// Which rooms own their files on this machine (#272).
    locks: crate::room_lock::Locks,
    /// Request ids the execute endpoint has queued.
    pub(crate) requests: crate::execute::Requests,
}

impl Default for Collab {
    fn default() -> Self {
        Collab::new(FileIds::in_memory(), None)
    }
}

impl Collab {
    /// Rooms whose file ids come from `ids`, for contents rooted at `root`
    /// on disk (watched for changes) when given.
    pub fn new(ids: FileIds, root: Option<PathBuf>) -> Self {
        Collab {
            session_id: uuid::Uuid::new_v4().to_string(),
            ids: Arc::new(ids),
            root,
            watcher: Watcher::new(),
            rooms: Mutex::default(),
            moves: AtomicU64::new(0),
            move_lock: tokio::sync::RwLock::new(()),
            next_conn: AtomicU64::new(1),
            cleanup_delay: CLEANUP_DELAY,
            heartbeat: (PING_INTERVAL, IDLE_TIMEOUT),
            locks: Default::default(),
            requests: Default::default(),
        }
    }

    /// Rooms take a lock in `dir` (a directory in the Jupyter runtime dir)
    /// to own their files, so another server's room on the same file is
    /// read-only. Without this, as in an embedding app, rooms don't lock.
    pub fn with_room_locks(mut self, dir: PathBuf) -> Self {
        self.locks = crate::room_lock::Locks::new(Some(dir));
        self
    }

    /// Try to own the notebook at `path` (relative to the root).
    fn acquire(&self, path: &str) -> crate::room_lock::Acquire {
        match &self.root {
            Some(root) => self.locks.acquire(&root.join(path)),
            None => crate::room_lock::Acquire::Unavailable,
        }
    }

    /// Whether a room in another server owns `path`, so this server can
    /// only read it.
    pub(crate) fn owned_elsewhere(&self, path: &str) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| self.locks.owned_elsewhere(&root.join(path)))
    }

    /// How many rooms are open.
    pub fn open_rooms(&self) -> usize {
        self.rooms.lock().expect("rooms").len()
    }

    /// Keep rooms with no clients for `delay` instead of a minute, so
    /// tests can see rooms retire.
    #[doc(hidden)]
    pub fn with_cleanup_delay(mut self, delay: Duration) -> Self {
        self.cleanup_delay = delay;
        self
    }

    /// Ping sockets every `ping` and drop one silent for `idle`, instead
    /// of every 30 and 90 seconds, so tests can see a half-open
    /// connection dropped.
    #[doc(hidden)]
    pub fn with_heartbeat(mut self, ping: Duration, idle: Duration) -> Self {
        self.heartbeat = (ping, idle);
        self
    }

    /// Run `f` on the file ids off the async workers: with a database,
    /// each call is a SQLite query, which can wait on another server's
    /// lock.
    async fn with_ids<T: Send + 'static>(
        &self,
        f: impl FnOnce(&FileIds) -> T + Send + 'static,
    ) -> T {
        let ids = self.ids.clone();
        tokio::task::spawn_blocking(move || f(&ids))
            .await
            .expect("file ids task")
    }

    /// The host API grant stored for the notebook at `path`, if any.
    pub async fn grant(&self, path: &str) -> Option<String> {
        let path = path.to_string();
        self.with_ids(move |ids| ids.grant(&ids.get(&path)?)).await
    }

    /// The id for `path`, made the first time; and whether it was new.
    async fn index(&self, path: &str) -> (String, bool) {
        let path = path.to_string();
        self.with_ids(move |ids| ids.index(&path)).await
    }

    /// The id for the file at `path`, as [`Collab::index`], if `contents`
    /// has one matching `options` (a notebook, say), or the contents
    /// error. Under [`Collab::begin_move`]'s lock for reading, so no
    /// rename or delete through the contents API lands between the check
    /// and the id: a path that is gone never gets one.
    pub(crate) async fn index_existing(
        &self,
        contents: &Arc<dyn Contents>,
        path: &str,
        options: GetOptions,
    ) -> ganymede_contents::Result<(String, bool)> {
        // The SQLite insert runs under the guard too, so a busy shared
        // file-id database (up to its 5 s `busy_timeout`) holds back a
        // queued rename or delete, and rooms behind that. Releasing the
        // guard before the insert would let a gone path get an id again.
        let _no_move = self.move_lock.read().await;
        contents.get(path, options).await?;
        Ok(self.index(path).await)
    }

    /// Deletes `path` through `contents` and drops its ids, under
    /// [`Collab::begin_move`]'s lock, so no id is made for the path
    /// between the delete and [`Collab::removed`] (#574). Runs to the end
    /// even if the caller is dropped, as a request is when its client
    /// disconnects, so the ids never keep a deleted path (#586).
    pub(crate) async fn delete(
        self: &Arc<Self>,
        contents: &Arc<dyn Contents>,
        path: &str,
    ) -> ganymede_contents::Result<()> {
        let (collab, contents, path) = (self.clone(), contents.clone(), path.to_string());
        detached(async move {
            let _deleting = collab.begin_move().await;
            contents.delete(&path).await?;
            collab.removed(&path).await;
            Ok(())
        })
        .await
    }

    /// Renames `old` to `new` through `contents`, and the ids and open
    /// rooms follow, under [`Collab::begin_move`]'s lock so no room opens
    /// on the old path in between (#522). Runs to the end even if the
    /// caller is dropped, as [`Collab::delete`] does (#586).
    pub(crate) async fn rename(
        self: &Arc<Self>,
        contents: &Arc<dyn Contents>,
        old: &str,
        new: &str,
    ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
        let (collab, contents) = (self.clone(), contents.clone());
        let (old, new) = (old.to_string(), new.to_string());
        detached(async move {
            let _moving = collab.begin_move().await;
            let model = contents.rename(&old, &new).await?;
            collab.moved(&old, &model.path).await;
            Ok(model)
        })
        .await
    }

    /// The path a file id names now.
    async fn path_of(&self, id: &str) -> Option<String> {
        let id = id.to_string();
        self.with_ids(move |ids| ids.path(&id)).await
    }

    /// A rename through the contents API is about to change the disk:
    /// until the returned guard drops, after [`Collab::moved`], no room
    /// opens, so none opens on the old path between the disk rename and
    /// the ids following it (#522). Dropping it without `moved`, when the
    /// rename failed, is fine: the path hasn't changed. A delete holds it
    /// the same way, until [`Collab::removed`] (#574).
    pub(crate) async fn begin_move(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.move_lock.write().await
    }

    /// `old` (a file or directory) was renamed to `new` through the
    /// contents API: ids follow, and open rooms take the new path.
    pub async fn moved(&self, old: &str, new: &str) {
        let (o, n) = (old.to_string(), new.to_string());
        self.with_ids(move |ids| ids.moved(&o, &n)).await;
        let rooms = self.rooms.lock().expect("rooms");
        self.moves.fetch_add(1, Ordering::SeqCst);
        for room in rooms.values() {
            let _ = room.send(Cmd::Moved(old.to_string(), new.to_string()));
        }
    }

    /// The notebook at `path` as its room has it, with cell ids, when a
    /// room is open on it; `None` otherwise. Opens no room and makes no
    /// file id, so a search can read many notebooks cheaply.
    pub(crate) async fn open_notebook(&self, path: &str) -> Option<Notebook> {
        let p = path.to_string();
        let id = self.with_ids(move |ids| ids.get(&p)).await?;
        let room = self
            .rooms
            .lock()
            .expect("rooms")
            .get(&format!("json:notebook:{id}"))
            .cloned()?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        room.send(Cmd::With(Box::new(move |room: &mut Room| {
            let _ = tx.send(room.notebook_with_ids());
            ganymede_documents::Effects::default()
        })))
        .ok()?;
        rx.await.ok()??.ok()
    }

    /// The contents root on disk, when the contents are a local directory.
    pub(crate) fn root(&self) -> Option<&std::path::Path> {
        self.root.as_deref()
    }

    /// `path` (a file or directory) was deleted through the contents
    /// API: its ids go, and rooms on it close their clients.
    pub async fn removed(&self, path: &str) {
        let p = path.to_string();
        self.with_ids(move |ids| ids.removed(&p)).await;
        let rooms = self.rooms.lock().expect("rooms");
        self.moves.fetch_add(1, Ordering::SeqCst);
        for room in rooms.values() {
            let _ = room.send(Cmd::Removed(path.to_string()));
        }
    }

    /// Saves every room with unsaved edits, as the server stops, so edits
    /// made within the save delay aren't lost.
    pub async fn flush_all(&self) {
        let waits: Vec<_> = self
            .rooms
            .lock()
            .expect("rooms")
            .values()
            .filter_map(|room| {
                let (tx, rx) = tokio::sync::oneshot::channel();
                room.send(Cmd::Flush(tx)).ok().map(|()| rx)
            })
            .collect();
        for rx in waits {
            let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        }
    }

    fn watch(&self, path: &str) -> Option<(Watch, mpsc::UnboundedReceiver<()>)> {
        let file = self.root.as_ref()?.join(path);
        self.watcher.watch(&file)
    }
}

pub(crate) enum Cmd {
    Join(ConnId, ClientTx),
    Frame(ConnId, Vec<u8>),
    Leave(ConnId),
    /// A rename through the contents API: old and new path.
    Moved(String, String),
    /// A delete through the contents API: the path (a file or directory).
    Removed(String),
    /// Server-side execution; see [`crate::execute`].
    Exec(crate::execute::RoomCmd),
    /// Run a function on the room, as `ganymede mcp` does to read and edit
    /// notebooks.
    With(Box<dyn FnOnce(&mut Room) -> ganymede_documents::Effects + Send>),
    /// Save now if there are unsaved edits; answered when done.
    Flush(tokio::sync::oneshot::Sender<()>),
}

pub(crate) enum Out {
    Frame(Vec<u8>),
    Close(u16, String),
}

/// A client's outgoing queue, bounded at [`CLIENT_QUEUE_LIMIT`] frames
/// and at `budget` bytes of frames (#472): each client holds its own copy
/// of every frame, so large outputs would fill memory long before the
/// frame count. A send that finds it full drops nothing silently: it
/// tells the client's socket task to close, and the client leaves the
/// room.
#[derive(Clone)]
pub(crate) struct ClientTx {
    tx: mpsc::Sender<Out>,
    full: Arc<Full>,
    budget: usize,
}

/// Raised when a client's queue is full: the socket task closes, with
/// the room's own close if one couldn't be queued, else 1008.
#[derive(Default)]
pub(crate) struct Full {
    notify: tokio::sync::Notify,
    close: Mutex<Option<(u16, String)>>,
    /// Bytes of frames queued and not yet taken by the socket task.
    queued: AtomicUsize,
}

impl Full {
    /// The socket task took `frame` off the queue.
    fn taken(&self, frame: &[u8]) {
        self.queued.fetch_sub(frame.len(), Ordering::SeqCst);
    }

    /// Wait until raised; the close code and reason to send.
    async fn raised(&self) -> (u16, String) {
        self.notify.notified().await;
        self.close
            .lock()
            .expect("close")
            .take()
            .unwrap_or_else(|| (1008, format!("fell {CLIENT_QUEUE_LIMIT} messages behind")))
    }
}

impl ClientTx {
    /// A queue of at most [`CLIENT_QUEUE_LIMIT`] frames and `budget`
    /// bytes of them (beyond one frame, which always fits an empty queue).
    fn new(budget: usize) -> (ClientTx, mpsc::Receiver<Out>, Arc<Full>) {
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE_LIMIT);
        let full = Arc::new(Full::default());
        (
            ClientTx {
                tx,
                full: full.clone(),
                budget,
            },
            rx,
            full,
        )
    }

    /// Queue `out`; a client gone or too far behind is not waited for. A
    /// close that finds the queue full goes around it.
    fn send(&self, out: Out) {
        let len = match &out {
            Out::Frame(f) => f.len(),
            Out::Close(..) => 0,
        };
        let before = self.full.queued.fetch_add(len, Ordering::SeqCst);
        let over = before > 0 && before.saturating_add(len) > self.budget;
        let sent = if over {
            Err(mpsc::error::TrySendError::Full(out))
        } else {
            self.tx.try_send(out)
        };
        let Err(e) = sent else { return };
        self.full.queued.fetch_sub(len, Ordering::SeqCst);
        // A client whose socket task is gone holds nothing.
        let mpsc::error::TrySendError::Full(out) = e else {
            return;
        };
        let mut close = self.full.close.lock().expect("close");
        match out {
            Out::Close(code, reason) => *close = Some((code, reason)),
            Out::Frame(_) if over => {
                close.get_or_insert_with(|| (1008, format!("fell {} bytes behind", self.budget)));
            }
            Out::Frame(_) => {}
        }
        drop(close);
        self.full.notify.notify_one();
    }
}

#[derive(Deserialize, Default)]
pub struct SessionBody {
    format: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

pub async fn session(
    State(state): State<AppState>,
    Path(path): Path<String>,
    LenientJson(body): LenientJson<SessionBody>,
) -> Result<Response, HttpError> {
    let path = ganymede_contents::normalize_api_path(&path);
    let (id, new) = state
        .collab
        .index_existing(&state.contents, &path, GetOptions::default())
        .await
        .map_err(|e| match e {
            ganymede_contents::Error::NotFound(_) => {
                HttpError(404, format!("File {path:?} does not exist"))
            }
            e => HttpError::from(e),
        })?;
    let status = if new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(json!({
            "format": body.format,
            "type": body.kind,
            "fileId": id,
            "sessionId": state.collab.session_id,
        })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct FileIdQuery {
    path: Option<String>,
    id: Option<String>,
}

/// `POST /api/fileid/index?path=`, `jupyter-server-documents`' way to get
/// a file's id (and make one): `{"id", "path"}`. nb-cli takes a server
/// that answers it for one that writes kernel outputs into rooms, and
/// runs cells through `POST /api/kernels/{id}/execute`.
pub async fn fileid_index(
    State(state): State<AppState>,
    Query(q): Query<FileIdQuery>,
) -> Result<Json<Value>, HttpError> {
    let path = q.path.ok_or_else(|| {
        HttpError(
            400,
            "'path' parameter was not provided in the request.".into(),
        )
    })?;
    let path = ganymede_contents::normalize_api_path(&path);
    let (id, _) = state
        .collab
        .index_existing(&state.contents, &path, GetOptions::default())
        .await?;
    Ok(Json(json!({"id": id, "path": path})))
}

/// `GET /api/fileid/id?path=`, as `jupyter_server_fileid`: the id of a
/// path that has one, or 404.
pub async fn fileid_id(
    State(state): State<AppState>,
    Query(q): Query<FileIdQuery>,
) -> Result<Json<Value>, HttpError> {
    let path = q.path.ok_or_else(|| {
        HttpError(
            400,
            "'path' parameter was not provided in the request.".into(),
        )
    })?;
    let path = ganymede_contents::normalize_api_path(&path);
    let p = path.clone();
    let id = state
        .collab
        .with_ids(move |ids| ids.get(&p))
        .await
        .ok_or_else(|| HttpError(404, format!("The ID for file, {path}, could not be found.")))?;
    Ok(Json(json!({"id": id, "path": path})))
}

/// `GET /api/fileid/path?id=`, as `jupyter_server_fileid`.
pub async fn fileid_path(
    State(state): State<AppState>,
    Query(q): Query<FileIdQuery>,
) -> Result<Json<Value>, HttpError> {
    let id = q.id.ok_or_else(|| {
        HttpError(
            400,
            "'id' parameter was not provided in the request.".into(),
        )
    })?;
    let path =
        state.collab.path_of(&id).await.ok_or_else(|| {
            HttpError(404, format!("The path for file, {id}, could not be found."))
        })?;
    Ok(Json(json!({"id": id, "path": path})))
}

#[derive(Deserialize)]
pub struct RoomQuery {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

pub async fn room(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<RoomQuery>,
    headers: HeaderMap,
) -> Response {
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    // No message larger than a notebook the contents API would take.
    // Browsers send each message as one frame, so the frame limit (16 MiB
    // by default) has to match.
    ws.max_message_size(state.max_body_size)
        .max_frame_size(state.max_body_size)
        .on_upgrade(move |socket| serve_socket(socket, state, room_id, query.session_id))
}

async fn serve_socket(
    mut socket: WebSocket,
    state: AppState,
    room_id: String,
    session_id: Option<String>,
) {
    let collab = state.collab.clone();
    let parts: Vec<&str> = room_id.splitn(3, ':').collect();
    let file_id = if let [format, kind, file_id] = parts[..] {
        if session_id
            .as_deref()
            .is_some_and(|s| s != collab.session_id)
        {
            let reason = json!({
                "reason": "unknown_session",
                "sessionId": session_id,
                "reloadable": true,
            });
            close(&mut socket, 1003, reason.to_string()).await;
            return;
        }
        if format != "json" || kind != "notebook" {
            // Text files (`jupyter_ydoc`'s YUnicode) are not served yet.
            close(
                &mut socket,
                4400,
                format!("{format}:{kind} documents are not supported"),
            )
            .await;
            return;
        }
        Some(file_id)
    } else {
        None
    };

    let conn = collab.next_conn.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx, full) = ClientTx::new(state.max_body_size);
    // An open room takes the join with no path lookup, so a client
    // joining one whose file was just deleted hears "File deleted".
    let join = Cmd::Join(conn, out_tx);
    let Some(room) = send_to_room(&collab, &state.contents, &room_id, file_id, join).await else {
        let id = file_id.unwrap_or_default();
        close(&mut socket, 4404, format!("Unknown file id {id}")).await;
        return;
    };
    tracing::debug!(room = %room_id, conn, "collaboration client connected");

    // Every message from the client, a pong included, shows the
    // connection is alive; one silent for IDLE_TIMEOUT is dropped.
    let (ping_every, idle_timeout) = collab.heartbeat;
    let mut ping = tokio::time::interval(ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                last_heard = Instant::now();
                match incoming {
                    Some(Ok(WsMessage::Binary(b))) => {
                        if room.send(Cmd::Frame(conn, b.to_vec())).is_err() { break }
                    }
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Text(_))) => {}
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(e)) => { tracing::debug!("collaboration websocket error: {e}"); break }
                }
            },
            out = out_rx.recv() => match out {
                Some(Out::Frame(f)) => {
                    full.taken(&f);
                    let started = Instant::now();
                    if !send_within(&mut socket, WsMessage::Binary(f.into())).await { break }
                    last_heard = heard_after_write(last_heard, started.elapsed(), ping_every);
                }
                Some(Out::Close(code, reason)) => { close(&mut socket, code, reason).await; break }
                None => break,
            },
            (code, reason) = full.raised() => {
                tracing::info!(room = %room_id, conn, code, "collaboration client's queue is full; closing it");
                close(&mut socket, code, reason).await;
                break
            }
            _ = ping.tick() => {
                if last_heard.elapsed() >= idle_timeout {
                    tracing::debug!(room = %room_id, conn, "collaboration client silent; dropping it");
                    break
                }
                if !send_within(&mut socket, WsMessage::Ping(Default::default())).await { break }
            }
        }
    }
    let _ = room.send(Cmd::Leave(conn));
}

/// Send `cmd` to `room_id`, opening the room if needed, and return its
/// sender: the one way into a room, for websocket clients and for the
/// server's own callers. An open room takes `cmd` with no path lookup;
/// an entry whose task has stopped is replaced by a fresh room. Opening a
/// notebook room (`file_id` given) looks its path up holding
/// [`Collab::begin_move`]'s lock for reading, so no rename is between its
/// disk change and the ids; a rename or delete finished meanwhile changes
/// `moves`, and the path is looked up again. `None` when the file id is
/// gone.
async fn send_to_room(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    file_id: Option<&str>,
    mut cmd: Cmd,
) -> Option<mpsc::UnboundedSender<Cmd>> {
    loop {
        // Sends with the rooms lock held, so the room can't retire between
        // the lookup and the send.
        {
            let mut rooms = collab.rooms.lock().expect("rooms");
            if let Some(tx) = rooms.get(room_id) {
                match tx.send(cmd) {
                    Ok(()) => return Some(tx.clone()),
                    Err(mpsc::error::SendError(back)) => {
                        rooms.remove(room_id);
                        cmd = back;
                    }
                }
            }
            if file_id.is_none() {
                let tx = open_room(collab, &mut rooms, contents, room_id, None);
                tx.send(cmd).ok()?;
                return Some(tx);
            }
        }
        let id = file_id?;
        let _no_rename = collab.move_lock.read().await;
        let read = collab.moves.load(Ordering::SeqCst);
        let path = collab.path_of(id).await?;
        let mut rooms = collab.rooms.lock().expect("rooms");
        // The read lock keeps out renames and deletes through the contents
        // API; a change here could only come from a caller of `removed`
        // that skips `begin_move`. Look the id up again to be safe.
        if !rooms.contains_key(room_id) && collab.moves.load(Ordering::SeqCst) != read {
            continue;
        }
        let tx = open_room(collab, &mut rooms, contents, room_id, Some(path));
        match tx.send(cmd) {
            Ok(()) => return Some(tx),
            Err(mpsc::error::SendError(back)) => {
                rooms.remove(room_id);
                cmd = back;
            }
        }
    }
}

/// The room's task, started if it isn't running. Call with `rooms` locked,
/// and send to the room before unlocking, so it can't retire in between.
fn open_room(
    collab: &Arc<Collab>,
    rooms: &mut HashMap<String, mpsc::UnboundedSender<Cmd>>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    path: Option<String>,
) -> mpsc::UnboundedSender<Cmd> {
    rooms
        .entry(room_id.to_string())
        .or_insert_with(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(run_room(
                collab.clone(),
                contents.clone(),
                room_id.to_string(),
                path,
                tx.clone(),
                rx,
            ));
            tx
        })
        .clone()
}

/// Send `cmd` to a notebook room, opening it if needed: for callers other
/// than a websocket client, such as the execute endpoint. `None` when the
/// room id names no known notebook. Returns the room's sender.
pub(crate) async fn send_to_notebook_room(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    cmd: Cmd,
) -> Option<mpsc::UnboundedSender<Cmd>> {
    let parts: Vec<&str> = room_id.splitn(3, ':').collect();
    let ["json", "notebook", file_id] = parts[..] else {
        return None;
    };
    send_to_room(collab, contents, room_id, Some(file_id), cmd).await
}

/// Runs `f` on its own task and waits for it: dropping the wait leaves
/// `f` running to the end. A panic in `f` comes back as an I/O error.
async fn detached<T: Send + 'static>(
    f: impl std::future::Future<Output = ganymede_contents::Result<T>> + Send + 'static,
) -> ganymede_contents::Result<T> {
    tokio::spawn(f)
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string()).into()))
}

/// The room id of the notebook at `path`, or the contents error when no
/// notebook is there (nothing, or a directory): as the REST routes do, only an
/// existing file gets a file id, so a mistyped path leaves no row in the
/// id table.
pub(crate) async fn notebook_room_id(
    collab: &Collab,
    contents: &Arc<dyn Contents>,
    path: &str,
) -> ganymede_contents::Result<String> {
    let notebook = GetOptions {
        type_hint: Some(ContentType::Notebook),
        ..Default::default()
    };
    let (id, _) = collab.index_existing(contents, path, notebook).await?;
    Ok(format!("json:notebook:{id}"))
}

/// Run `f` on notebook room `room_id` (from [`notebook_room_id`]),
/// opening it if needed. `None` when the room can't be opened (the file
/// is gone or not a notebook).
pub(crate) async fn with_notebook<R: Send + 'static>(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    f: impl FnOnce(&mut Room) -> (R, ganymede_documents::Effects) + Send + 'static,
) -> Option<R> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cmd = Cmd::With(Box::new(move |room: &mut Room| {
        let (r, fx) = f(room);
        let _ = tx.send(r);
        fx
    }));
    send_to_notebook_room(collab, contents, room_id, cmd).await?;
    rx.await.ok()
}

/// A notebook room kept open, as a client keeps it while connected: the
/// room doesn't retire, so cell ids it made up for a notebook without
/// them (nbformat 4.4 and older) stay the same. Leaves on drop.
pub(crate) struct Hold {
    room: mpsc::UnboundedSender<Cmd>,
    conn: ConnId,
}

impl Hold {
    /// Whether the room is still running; it stops when its file can't
    /// be loaded.
    pub(crate) fn is_live(&self) -> bool {
        !self.room.is_closed()
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = self.room.send(Cmd::Leave(self.conn));
    }
}

/// Join notebook room `room_id` (from [`notebook_room_id`]) as a client
/// that never reads its frames. `None` when the room can't be opened.
pub(crate) async fn hold_notebook(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
) -> Option<Hold> {
    let conn = collab.next_conn.fetch_add(1, Ordering::Relaxed);
    // The receiver is dropped: frames sent to this client go nowhere.
    let (out, _, _) = ClientTx::new(usize::MAX);
    let room = send_to_notebook_room(collab, contents, room_id, Cmd::Join(conn, out)).await?;
    Some(Hold { room, conn })
}

async fn close(socket: &mut WebSocket, code: u16, reason: String) {
    let frame = WsMessage::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }));
    send_within(socket, frame).await;
}

/// Send `msg`, giving up after [`WRITE_TIMEOUT`]. False when the client
/// is gone or not reading.
async fn send_within(socket: &mut WebSocket, msg: WsMessage) -> bool {
    let limit = write_timeout(msg_len(&msg));
    matches!(
        tokio::time::timeout(limit, socket.send(msg)).await,
        Ok(Ok(()))
    )
}

/// When the client was last known alive, after a write that took
/// `took`. A write that kept going for a whole ping interval had the
/// client reading (its window kept opening), so it counts as hearing from
/// it; a quick one only filled a buffer and proves nothing.
fn heard_after_write(last_heard: Instant, took: Duration, ping_every: Duration) -> Instant {
    if took >= ping_every {
        Instant::now()
    } else {
        last_heard
    }
}

fn msg_len(msg: &WsMessage) -> usize {
    match msg {
        WsMessage::Binary(b) | WsMessage::Ping(b) | WsMessage::Pong(b) => b.len(),
        WsMessage::Text(t) => t.len(),
        WsMessage::Close(_) => 0,
    }
}

/// [`WRITE_TIMEOUT`], plus a second for every [`MIN_WRITE_RATE`] bytes.
fn write_timeout(len: usize) -> Duration {
    WRITE_TIMEOUT + Duration::from_secs(len as u64 / MIN_WRITE_RATE)
}

/// A room's file: what was last read or written, to tell our own writes
/// from someone else's and to merge theirs.
struct Disk {
    path: String,
    hash: String,
    last_modified: String,
    /// The notebook as last read or written: the base of a merge.
    base: Notebook,
    /// The file as last read, when nobody had signed it;
    /// [`Disk::track`] takes its outputs in.
    read_untrusted: Option<Notebook>,
    /// Digests of outputs read from a file nobody signed (#597). A cell
    /// holding one of them is untrusted, whatever its id (a cell cut and
    /// pasted gets a new one), until a run here replaces its outputs or
    /// someone trusts the file. Other outputs came from a run here or a
    /// signed file.
    untrusted: HashSet<String>,
}

impl Disk {
    /// Take in the outputs of the file as last read, when it was unsigned,
    /// leaving out those already in `before` (the file as the room last
    /// read or wrote it), which are the room's own or already counted.
    fn track(&mut self, before: Option<&Notebook>) {
        let Some(file) = self.read_untrusted.take() else {
            return;
        };
        let mut read = room_digests(&file);
        if let Some(before) = before {
            for d in room_digests(before) {
                read.remove(&d);
            }
        }
        self.untrusted.extend(read);
    }

    /// A run here replaced cell `id`'s outputs: what it holds now is
    /// trusted, here and in any other cell holding the same.
    fn ran(&mut self, room: &Room, id: &str) {
        if self.untrusted.is_empty() {
            return;
        }
        // Only this cell's outputs, serialized as the notebook would be.
        let Some(outputs) = room.cell_outputs(id) else {
            return;
        };
        for o in outputs.as_array().into_iter().flatten() {
            let o = serde_json::from_value::<ganymede_nbformat::Output>(o.clone())
                .ok()
                .and_then(|o| serde_json::to_value(o).ok())
                .unwrap_or_else(|| o.clone());
            self.untrusted.remove(&output_digest(&o));
        }
    }
}

/// The digests of every output in `nb`, as a room holds them: through
/// Yjs, which changes numbers and key order, so they match the room's.
fn room_digests(nb: &Notebook) -> HashSet<String> {
    match Room::notebook_room(nb, "").notebook_with_ids() {
        Some(Ok(nb)) => cell_digests(&nb).into_iter().flat_map(|(_, d)| d).collect(),
        _ => HashSet::new(),
    }
}

/// Why a notebook didn't load.
struct LoadError {
    /// The file isn't there (deleted, perhaps just now).
    missing: bool,
    message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

async fn load(contents: &dyn Contents, path: &str) -> Result<(Notebook, Disk), LoadError> {
    let model = contents
        .get(
            path,
            GetOptions {
                content: true,
                type_hint: Some(ContentType::Notebook),
                format: None,
                hash: true,
            },
        )
        .await
        .map_err(|e| LoadError {
            missing: matches!(
                e,
                ganymede_contents::Error::NotFound(_) | ganymede_contents::Error::Hidden(_)
            ),
            message: e.to_string(),
        })?;
    let content = model.content.unwrap_or(Value::Null);
    let signed = is_signed(&content);
    let nb = Notebook::from_value(content).map_err(|e| LoadError {
        missing: false,
        message: e.to_string(),
    })?;
    let read_untrusted = (!signed).then(|| nb.clone());
    Ok((
        nb.clone(),
        Disk {
            path: path.to_string(),
            hash: model.hash.unwrap_or_default(),
            last_modified: model.last_modified,
            base: nb,
            read_untrusted,
            untrusted: HashSet::new(),
        },
    ))
}

/// Whether the contents manager found the file's signature valid: it
/// marks every code cell `trusted` (`mark_trusted_cells`). Without a
/// notary no cell is marked, and nothing is signed on save either.
fn is_signed(content: &Value) -> bool {
    content["cells"].as_array().is_some_and(|cells| {
        cells
            .iter()
            .filter(|c| c["cell_type"] == "code")
            .all(|c| c["metadata"]["trusted"] == Value::Bool(true))
    })
}

/// A digest of one output: its JSON with every object's keys sorted,
/// since the room gives them back in no set order.
fn output_digest(output: &Value) -> String {
    use sha2::Digest as _;
    fn canonical(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    canonical(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut text = String::new();
    canonical(output, &mut text);
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

/// Each code cell's id with the digests of its outputs.
fn cell_digests(nb: &Notebook) -> Vec<(String, HashSet<String>)> {
    let value = serde_json::to_value(nb).expect("a notebook serializes");
    value["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["cell_type"] == "code")
        .map(|c| {
            let id = c["id"].as_str().unwrap_or_default().to_string();
            let digests = c["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(output_digest)
                .collect();
            (id, digests)
        })
        .collect()
}

/// Mark each code cell of `content` (the room's notebook) trusted
/// unless it holds an output among `untrusted`, read from an unsigned
/// file, so the contents manager signs the notebook when every cell is
/// (jupyter_server's `check_and_sign`). Outputs a run here wrote are
/// trusted, as a cell a person runs in JupyterLab is: through the execute
/// endpoint, MCP, or a client writing what its kernel sent, which needs
/// the server's token as a signed `PUT /api/contents` does. Returns
/// whether every code cell is trusted.
fn mark_trusted(content: &mut Value, untrusted: &HashSet<String>) -> bool {
    let mut all = true;
    let code = content["cells"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .filter(|c| c["cell_type"] == "code");
    for cell in code {
        let trusted = untrusted.is_empty()
            || !cell["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|o| untrusted.contains(&output_digest(o)));
        all &= trusted;
        if let Some(Value::Object(metadata)) = cell.get_mut("metadata") {
            metadata.insert("trusted".into(), Value::Bool(trusted));
        }
    }
    all
}

async fn run_room(
    collab: Arc<Collab>,
    contents: Arc<dyn Contents>,
    room_id: String,
    path: Option<String>,
    me: mpsc::UnboundedSender<Cmd>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let mut clients: HashMap<ConnId, ClientTx> = HashMap::new();
    let (mut room, mut disk) = match &path {
        None => (Room::transient(), None),
        Some(path) => match load(contents.as_ref(), path).await {
            Ok((nb, mut disk)) => {
                let mut room = Room::notebook_room(&nb, path);
                room.saved(0, &disk.hash);
                disk.track(None);
                (room, Some(disk))
            }
            Err(e) => {
                tracing::warn!(room = %room_id, "cannot load {path}: {e}");
                // A file deleted between resolving its id and opening the
                // room answers as the deleted file's room does.
                let code = if e.missing { 4404 } else { 4500 };
                let reason = if e.missing {
                    format!("File deleted: {path}")
                } else {
                    format!("Error initializing: {path}.")
                };
                close_room(
                    &collab,
                    &room_id,
                    &mut rx,
                    code,
                    &reason,
                    &format!("cannot load {path}: {e}"),
                );
                return;
            }
        },
    };
    let mut watch = path.as_deref().and_then(|p| collab.watch(p));
    // Held while the room is open; `None` while read-only or not locking.
    let mut lock = None;
    // Whether to try for the lock again on the tick: while read-only, or
    // while another room of this server still holds it.
    let mut lock_retry = false;
    if let Some(p) = &path {
        lock_retry = take_lock(&collab, &room_id, p, &mut room, &mut lock);
    }
    let mut last_lock_try = Instant::now();
    tracing::info!(room = %room_id, path = ?path, watched = watch.is_some(), read_only = room.is_read_only(), "room opened");

    let mut last_edit: Option<Instant> = None;
    // Saves that failed in a row, for the retry's backoff.
    let mut save_failures = 0;
    // A room opened for execution, with no client, retires like one whose
    // clients left.
    let mut empty_since: Option<Instant> = Some(Instant::now());
    let mut exec = crate::execute::RoomExec::new(me);
    // When file events last and first arrived, not yet looked at.
    let mut disk_event: Option<Instant> = None;
    let mut first_disk_event: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut last_poll = Instant::now();
    // A command taken off the queue while merging appends, to handle next.
    let mut next: Option<Cmd> = None;
    loop {
        tokio::select! {
            cmd = async { match next.take() { Some(c) => Some(c), None => rx.recv().await } } => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Join(conn, out) => {
                        clients.insert(conn, out);
                        empty_since = None;
                        let fx = room.connect(conn);
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Frame(conn, data) => match room.receive(conn, &data) {
                        Ok(fx) => {
                            if fx.changed { last_edit = Some(Instant::now()) }
                            let save = fx.save;
                            dispatch(&clients, fx.send);
                            if let Some((conn, id)) = save {
                                let status = match disk.as_mut() {
                                    Some(d) => save_room(contents.as_ref(), &mut room, d, &clients, true).await,
                                    None => SaveStatus::Skipped,
                                };
                                after_save(status, &mut last_edit, &mut save_failures);
                                if let Some(out) = clients.get(&conn) {
                                    out.send(Out::Frame(Room::save_reply(id, status)));
                                }
                            }
                        }
                        Err(e) => tracing::debug!(room = %room_id, conn, "bad frame: {e}"),
                    },
                    Cmd::Leave(conn) => {
                        clients.remove(&conn);
                        let fx = room.disconnect(conn);
                        dispatch(&clients, fx.send);
                        if clients.is_empty() { empty_since = Some(Instant::now()) }
                    }
                    Cmd::Moved(old, new) => {
                        let Some(d) = disk.as_mut() else { continue };
                        let Some(rest) = moved_path(&d.path, &old, &new) else { continue };
                        tracing::info!(room = %room_id, "{} moved to {rest}", d.path);
                        d.path = rest;
                        let fx = room.set_path(&d.path);
                        dispatch(&clients, fx.send);
                        drop(watch.take());
                        watch = collab.watch(&d.path);
                        // The lock names the old path, and the room may not
                        // get the new one: save first (#437).
                        save_before_relock(contents.as_ref(), &mut room, d, &clients, &mut last_edit, &mut save_failures).await;
                        drop(lock.take());
                        lock_retry = take_lock(&collab, &room_id, &d.path, &mut room, &mut lock);
                    }
                    Cmd::Removed(gone) => {
                        let Some(d) = disk.as_ref() else { continue };
                        if !is_under(&d.path, &gone) { continue }
                        // As a reconnect would find: the file id is gone.
                        tracing::info!(room = %room_id, "{} deleted; closing the room", d.path);
                        let reason = format!("File deleted: {}", d.path);
                        for out in clients.values() {
                            out.send(Out::Close(4404, reason.clone()));
                        }
                        close_room(&collab, &room_id, &mut rx, 4404, &reason, &reason);
                        break;
                    }
                    Cmd::With(f) => {
                        let fx = f(&mut room);
                        if fx.changed { last_edit = Some(Instant::now()) }
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Exec(crate::execute::RoomCmd::Append { cell_id, mut outputs }) => {
                        // Outputs queued for the same cell go in one edit, so
                        // a fast print loop makes few document edits when the
                        // room falls behind.
                        loop {
                            match rx.try_recv() {
                                Ok(Cmd::Exec(crate::execute::RoomCmd::Append { cell_id: c, outputs: more }))
                                    if c == cell_id => outputs.extend(more),
                                Ok(other) => { next = Some(other); break }
                                Err(_) => break,
                            }
                        }
                        let cmd = crate::execute::RoomCmd::Append { cell_id, outputs };
                        let fx = exec.handle(&mut room, cmd);
                        if fx.changed { last_edit = Some(Instant::now()) }
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Exec(cmd) => {
                        let ran = cmd.ran_cell().map(str::to_owned);
                        let fx = exec.handle(&mut room, cmd);
                        if let (Some(d), Some(id)) = (disk.as_mut(), ran) { d.ran(&room, &id) }
                        if fx.changed { last_edit = Some(Instant::now()) }
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Flush(done) => {
                        let status = match disk.as_mut() {
                            Some(d) if room.is_dirty() => {
                                save_room(contents.as_ref(), &mut room, d, &clients, false).await
                            }
                            _ => SaveStatus::Skipped,
                        };
                        after_save(status, &mut last_edit, &mut save_failures);
                        let _ = done.send(());
                    }
                }
            }
            Some(()) = changed(&mut watch) => {
                let now = Instant::now();
                disk_event = Some(now);
                first_disk_event.get_or_insert(now);
            }
            _ = tick.tick() => {
                let Some(d) = disk.as_mut() else {
                    if clients.is_empty() && next.is_none() && retire(&collab, &room_id, &mut rx, &mut lock) { break }
                    continue;
                };
                if lock_retry && last_lock_try.elapsed() >= POLL_INTERVAL {
                    last_lock_try = Instant::now();
                    // A room writable without the lock may find another
                    // server owns the file now: save first (#437).
                    save_before_relock(contents.as_ref(), &mut room, d, &clients, &mut last_edit, &mut save_failures).await;
                    lock_retry = take_lock(&collab, &room_id, &d.path, &mut room, &mut lock);
                }
                let poll_every = if watch.is_some() { WATCHED_POLL_INTERVAL } else { POLL_INTERVAL };
                let settled = disk_event.is_some_and(|t| t.elapsed() >= DEBOUNCE);
                if settled || first_disk_event.is_some_and(|t| t.elapsed() >= MAX_DEBOUNCE) {
                    disk_event = None;
                    first_disk_event = None;
                    last_poll = Instant::now();
                    let needs_save = reload(contents.as_ref(), &mut room, d, &clients).await.needs_save;
                    // Save the room's edits now, just after taking in the
                    // file, so the save rarely meets a newer one. Waiting
                    // for autosave instead starves it while the file keeps
                    // changing: each reload would push it back, and the
                    // next file event holds it off again.
                    if needs_save || (!settled && last_edit.is_some()) {
                        let status = save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                        after_save(status, &mut last_edit, &mut save_failures);
                    }
                } else if last_poll.elapsed() >= poll_every && disk_event.is_none() {
                    last_poll = Instant::now();
                    if poll(contents.as_ref(), &mut room, d, &clients).await {
                        // As after a reload.
                        let status = save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                        after_save(status, &mut last_edit, &mut save_failures);
                    }
                }
                if last_edit.is_some_and(|t| t.elapsed() >= SAVE_DELAY) && disk_event.is_none() {
                    let status = save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                    after_save(status, &mut last_edit, &mut save_failures);
                }
                if empty_since.is_some_and(|t| t.elapsed() >= collab.cleanup_delay) && exec.is_idle() {
                    // Save now unless the autosave already has one due, so a
                    // failing save keeps one retry clock: #353's backoff.
                    if room.is_dirty() && last_edit.is_none() {
                        let status = save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                        after_save(status, &mut last_edit, &mut save_failures);
                    }
                    // A failed save leaves the edits only in the room, so it
                    // stays open while the autosave retries (#359), unless
                    // its file is gone or the saves keep failing.
                    if room.is_dirty() && save_failures > 0 {
                        let gone = matches!(
                            contents.get(&d.path, GetOptions::default()).await,
                            Err(ganymede_contents::Error::NotFound(_)
                                | ganymede_contents::Error::Hidden(_))
                        );
                        if !gone && save_failures < GIVE_UP_AFTER {
                            tracing::warn!(
                                room = %room_id,
                                path = %d.path,
                                failures = save_failures,
                                "no clients left, but the room's edits aren't saved; keeping it open"
                            );
                            empty_since = Some(Instant::now());
                            continue;
                        }
                        tracing::warn!(
                            room = %room_id,
                            path = %d.path,
                            failures = save_failures,
                            "closing the room with unsaved edits: {}",
                            if gone { "its file is gone" } else { "its saves keep failing" }
                        );
                    } else if room.is_dirty() && !room.is_read_only() {
                        // An edit the autosave hasn't reached yet.
                        empty_since = Some(Instant::now());
                        continue;
                    } else if room.is_dirty() {
                        // Another server owns the file now, so these
                        // edits can't be saved here; keeping the room
                        // wouldn't change that (#437).
                        tracing::warn!(
                            room = %room_id,
                            path = %d.path,
                            "closing a read-only room with edits it couldn't save"
                        );
                    }
                    if next.is_none() && retire(&collab, &room_id, &mut rx, &mut lock) { break }
                    empty_since = Some(Instant::now());
                }
            }
        }
    }
    tracing::info!(room = %room_id, "room closed");
}

/// Own the room's file, or make the room read-only while another server's
/// room does (#272). Returns whether to try again on the tick.
fn take_lock(
    collab: &Collab,
    room_id: &str,
    path: &str,
    room: &mut Room,
    lock: &mut Option<crate::room_lock::RoomLock>,
) -> bool {
    use crate::room_lock::Acquire;
    let was = room.is_read_only();
    let retry = match collab.acquire(path) {
        Acquire::Held(held) => {
            *lock = Some(held);
            room.set_read_only(false);
            false
        }
        Acquire::Elsewhere => {
            room.set_read_only(true);
            true
        }
        Acquire::HeldHere => {
            room.set_read_only(false);
            true
        }
        Acquire::Unavailable => {
            room.set_read_only(false);
            // A read-only room keeps trying: the file may be missing for a
            // moment while another program replaces it.
            was
        }
    };
    match (was, room.is_read_only()) {
        (false, true) if room.is_dirty() => tracing::warn!(
            room = %room_id,
            "{path} is open in another server, which saves it; this room is read-only, \
             and its edits since the last save are lost"
        ),
        (false, true) => tracing::info!(
            room = %room_id,
            "{path} is open in another server, which saves it; this room is read-only"
        ),
        (true, false) => tracing::info!(room = %room_id, "{path} is this room's to save now"),
        _ => {}
    }
    retry
}

/// Save the room's edits before a lock check that may make it read-only,
/// which would leave them unsaved (#437). A read-only room saves nothing.
async fn save_before_relock(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
    last_edit: &mut Option<Instant>,
    failures: &mut u32,
) {
    if room.is_dirty() && !room.is_read_only() {
        let status = save_room(contents, room, disk, clients, false).await;
        after_save(status, last_edit, failures);
    }
}

/// Settle the pending autosave after a save. A failed save leaves the
/// room's edits unsaved, so it schedules another try instead of waiting
/// for the next edit (#353), later each time it keeps failing: 1 s, 2 s,
/// 4 s, ... up to about a minute.
fn after_save(status: SaveStatus, last_edit: &mut Option<Instant>, failures: &mut u32) {
    if status == SaveStatus::Failed {
        let backoff = SAVE_DELAY * ((1u32 << (*failures).min(6)) - 1);
        *failures += 1;
        // `elapsed` on an instant still ahead is zero, so the autosave
        // waits out the backoff and then the save delay.
        *last_edit = Some(Instant::now() + backoff);
    } else {
        *failures = 0;
        *last_edit = None;
    }
}

/// The next file event, or never when the room isn't watched.
async fn changed(watch: &mut Option<(Watch, mpsc::UnboundedReceiver<()>)>) -> Option<()> {
    match watch {
        Some((_, rx)) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// `path` after `old` was renamed to `new`, if it was `old` or under it.
fn moved_path(path: &str, old: &str, new: &str) -> Option<String> {
    // The root can't be renamed; `path_under` would match every path.
    if old.is_empty() {
        return None;
    }
    ganymede_contents::path_under(path, old).map(|rest| format!("{new}{rest}"))
}

/// `path` is `dir` or under it.
fn is_under(path: &str, dir: &str) -> bool {
    !dir.is_empty() && ganymede_contents::path_under(path, dir).is_some()
}

/// Remove a room that can't go on (its file failed to load or was
/// deleted) from the map, and turn away what was queued for it: clients
/// joining close with `code`, executions get `exec_reason`.
fn close_room(
    collab: &Collab,
    room_id: &str,
    rx: &mut mpsc::UnboundedReceiver<Cmd>,
    code: u16,
    reason: &str,
    exec_reason: &str,
) {
    collab.rooms.lock().expect("rooms").remove(room_id);
    rx.close();
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::Join(_, out) => {
                out.send(Out::Close(code, reason.to_string()));
            }
            Cmd::Exec(cmd) => cmd.unavailable(exec_reason),
            Cmd::Flush(done) => {
                let _ = done.send(());
            }
            _ => {}
        }
    }
}

/// Remove the room from the map, unless a client is on its way in.
fn retire(
    collab: &Collab,
    room_id: &str,
    rx: &mut mpsc::UnboundedReceiver<Cmd>,
    lock: &mut Option<crate::room_lock::RoomLock>,
) -> bool {
    let mut rooms = collab.rooms.lock().expect("rooms");
    if !rx.is_empty() {
        return false;
    }
    rooms.remove(room_id);
    // Release the file while `rooms` is still locked: a room opened on it
    // after this finds the lock free, not held by this server (#400).
    drop(lock.take());
    true
}

fn dispatch(clients: &HashMap<ConnId, ClientTx>, send: Vec<(Target, Vec<u8>)>) {
    for (target, frame) in send {
        for (conn, out) in clients {
            let to = match target {
                Target::One(c) => *conn == c,
                Target::Others(c) => *conn != c,
                Target::All => true,
            };
            if to {
                out.send(Out::Frame(frame.clone()));
            }
        }
    }
}

/// Before a save while some outputs are untrusted: forget them if someone
/// trusted the file as the room last read or wrote it (`ganymede trust`,
/// a frontend's Trust Notebook). Every such save looks: one that missed
/// the signature would overwrite the trusted file unsigned.
async fn settle_trust(contents: &dyn Contents, disk: &mut Disk) {
    let opts = GetOptions {
        content: true,
        type_hint: Some(ContentType::Notebook),
        format: None,
        hash: true,
    };
    if let Ok(model) = contents.get(&disk.path, opts).await {
        let same = model.hash.as_deref() == Some(disk.hash.as_str());
        if same && model.content.as_ref().is_some_and(is_signed) {
            disk.untrusted.clear();
        }
    }
}

/// Write the room to its file, if it changed or `force`. When the file
/// changed since it was last read, merge it in first and save the merge.
/// A forced save (a client's `save`) whose merge let the file win a cell
/// the room had changed saves the merge but answers `Failed`, since the
/// client's edit to that cell didn't make it into the file.
async fn save_room(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
    force: bool,
) -> SaveStatus {
    // Another server's room saves this file (#272).
    if room.is_read_only() {
        return SaveStatus::Skipped;
    }
    // Each round either saves or merges a newer file; give up if another
    // program keeps writing.
    let mut lost = false;
    for _ in 0..3 {
        if !room.is_dirty() && !force {
            return SaveStatus::Skipped;
        }
        let generation = room.generation();
        let nb = match room.notebook() {
            Some(Ok(nb)) => nb,
            Some(Err(e)) => {
                tracing::warn!(path = %disk.path, "room content is not a notebook: {e}");
                return SaveStatus::Failed;
            }
            None => return SaveStatus::Skipped,
        };
        let mut content = serde_json::to_value(&nb).expect("a notebook serializes");
        if !disk.untrusted.is_empty() {
            settle_trust(contents, disk).await;
        }
        let trusted = mark_trusted(&mut content, &disk.untrusted);
        let request =
            SaveRequest::notebook(content).with_if_match(IfMatch::Hashes(vec![disk.hash.clone()]));
        match contents.save(&disk.path, request).await {
            Ok(model) => {
                disk.hash = model.hash.unwrap_or_default();
                disk.last_modified = model.last_modified;
                disk.base = nb;
                if !trusted {
                    tracing::debug!(path = %disk.path, "saved unsigned: outputs from an unsigned file");
                }
                let fx = room.saved(generation, &disk.hash);
                dispatch(clients, fx.send);
                tracing::debug!(path = %disk.path, "room saved");
                if force && lost {
                    return SaveStatus::Failed;
                }
                return SaveStatus::Success;
            }
            Err(e) if e.status() == 412 => {
                tracing::info!(path = %disk.path, "file changed on disk; merging before saving");
                lost |= reload(contents, room, disk, clients).await.lost;
            }
            Err(e) => {
                tracing::warn!(path = %disk.path, "room save failed: {e}");
                return SaveStatus::Failed;
            }
        }
    }
    SaveStatus::Failed
}

/// Merge the file in when it changed and it was not our write. True when
/// the room needs saving after.
async fn poll(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
) -> bool {
    let Ok(model) = contents.get(&disk.path, GetOptions::default()).await else {
        return false; // gone or unreadable: keep the room as it is
    };
    if model.last_modified == disk.last_modified {
        return false;
    }
    reload(contents, room, disk, clients).await.needs_save
}

/// What [`reload`] did.
#[derive(Default)]
struct Reloaded {
    /// The room holds a merge that needs saving.
    needs_save: bool,
    /// The file won a cell the room had changed.
    lost: bool,
}

/// Read the file and merge it into the room, unless it is what we last
/// read or wrote.
async fn reload(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
) -> Reloaded {
    // The hash first, so a file event for our own save, or a file touched
    // but not changed, doesn't parse the notebook.
    let stat = GetOptions {
        hash: true,
        ..GetOptions::default()
    };
    if let Ok(model) = contents.get(&disk.path, stat).await {
        if model.hash.as_deref() == Some(disk.hash.as_str()) {
            disk.last_modified = model.last_modified;
            return Reloaded::default();
        }
    }
    match load(contents, &disk.path).await {
        Ok((nb, fresh)) => {
            disk.last_modified = fresh.last_modified;
            if fresh.hash == disk.hash {
                return Reloaded::default(); // touched, not changed (or our own write)
            }
            disk.hash = fresh.hash;
            let (fx, conflicts) = room.merge_disk(&disk.base, &nb, &disk.hash);
            // Another program wrote the file: what it brought is trusted
            // only if it signed it. What was untrusted before stays so.
            disk.read_untrusted = fresh.read_untrusted;
            let before = std::mem::replace(&mut disk.base, nb);
            disk.track(Some(&before));
            if conflicts.is_empty() {
                tracing::info!(path = %disk.path, "file changed on disk; merged into the room");
            } else {
                tracing::warn!(
                    path = %disk.path,
                    "file changed on disk; the room's edits to {} lost to it",
                    conflicts.join(", ")
                );
            }
            dispatch(clients, fx.send);
            Reloaded {
                needs_save: room.is_dirty(),
                lost: !conflicts.is_empty(),
            }
        }
        Err(e) => {
            tracing::warn!(path = %disk.path, "cannot reload: {e}");
            Reloaded::default()
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::{is_under, moved_path};

    #[test]
    fn the_root_matches_nothing() {
        assert_eq!(moved_path("a.ipynb", "", "b"), None);
        assert!(!is_under("a.ipynb", ""));
        assert_eq!(
            moved_path("d/a.ipynb", "d", "e").as_deref(),
            Some("e/a.ipynb")
        );
        assert!(is_under("d/a.ipynb", "d"));
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    /// A room that finds the lock held by another room of this server is
    /// writable without it and asks to try again, then takes it once the
    /// other room lets go.
    #[test]
    fn a_room_finding_the_lock_held_here_takes_it_later() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let collab = Collab::new(FileIds::in_memory(), dir.path().canonicalize().ok())
            .with_room_locks(dir.path().join("locks"));
        let crate::room_lock::Acquire::Held(old) = collab.acquire("a.ipynb") else {
            panic!("the old room owns it")
        };
        let mut room = Room::transient();
        let mut lock = None;
        assert!(take_lock(&collab, "r", "a.ipynb", &mut room, &mut lock));
        assert!(!room.is_read_only() && lock.is_none());

        drop(old);
        assert!(!take_lock(&collab, "r", "a.ipynb", &mut room, &mut lock));
        assert!(!room.is_read_only() && lock.is_some());
        // Now another server finds the file owned.
        let other = Collab::new(FileIds::in_memory(), dir.path().canonicalize().ok())
            .with_room_locks(dir.path().join("locks"));
        assert!(other.owned_elsewhere("a.ipynb"));
    }

    /// A writable room that finds another server owns its file now saves
    /// its edits before it turns read-only, instead of losing them (#437).
    #[tokio::test]
    async fn a_room_saves_before_another_server_takes_the_file() {
        use ganymede_documents::yrs::{GetString, Map, Out, Text};
        let dir = tempfile::tempdir().unwrap();
        let nb = r#"{"cells":[{"cell_type":"markdown","id":"a","metadata":{},"source":["hello"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(dir.path().join("a.ipynb"), nb).unwrap();
        let root = dir.path().canonicalize().ok();
        let contents = ganymede_contents::FsContents::new(dir.path(), Default::default()).unwrap();
        let (loaded, mut disk) = load(&contents, "a.ipynb").await.ok().unwrap();
        let mut room = Room::notebook_room(&loaded, "a.ipynb");
        room.saved(0, &disk.hash);
        let (_, fx) = room.edit(|txn, roots| {
            let cell = ganymede_documents::schema::find_cell(txn, roots, "a").unwrap();
            let Some(Out::YText(t)) = cell.get(txn, "source") else {
                panic!("no source")
            };
            let end = t.len(txn);
            t.insert(txn, end, " again");
            assert_eq!(t.get_string(txn), "hello again");
        });
        assert!(fx.changed && room.is_dirty());

        // Another server takes the file while this room is writable
        // without the lock.
        let other = Collab::new(FileIds::in_memory(), root.clone())
            .with_room_locks(dir.path().join("locks"));
        let crate::room_lock::Acquire::Held(_theirs) = other.acquire("a.ipynb") else {
            panic!("the other server owns it")
        };
        let collab =
            Collab::new(FileIds::in_memory(), root).with_room_locks(dir.path().join("locks"));
        let (mut last_edit, mut failures) = (Some(Instant::now()), 0);
        save_before_relock(
            &contents,
            &mut room,
            &mut disk,
            &HashMap::new(),
            &mut last_edit,
            &mut failures,
        )
        .await;
        let mut lock = None;
        assert!(take_lock(&collab, "r", "a.ipynb", &mut room, &mut lock));
        assert!(room.is_read_only());
        assert!(!room.is_dirty(), "the edit was saved first");
        let saved = std::fs::read_to_string(dir.path().join("a.ipynb")).unwrap();
        assert!(saved.contains("hello again"), "{saved}");
    }

    /// A retiring room releases its lock before another room on the file
    /// can open, so the new room takes it at once (#400).
    #[test]
    fn retiring_releases_the_lock_before_a_new_room_opens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let collab = Collab::new(FileIds::in_memory(), dir.path().canonicalize().ok())
            .with_room_locks(dir.path().join("locks"));
        let crate::room_lock::Acquire::Held(held) = collab.acquire("a.ipynb") else {
            panic!("the old room owns it")
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        collab.rooms.lock().unwrap().insert("r".into(), tx);
        let mut lock = Some(held);
        assert!(retire(&collab, "r", &mut rx, &mut lock));
        assert!(lock.is_none());

        let mut room = Room::transient();
        let mut new_lock = None;
        assert!(!take_lock(
            &collab,
            "r2",
            "a.ipynb",
            &mut room,
            &mut new_lock
        ));
        assert!(new_lock.is_some(), "the new room owns the file");
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    /// A client too far behind is told to go, and nothing waits on it.
    #[tokio::test]
    async fn a_full_client_queue_asks_the_client_to_close() {
        let (tx, mut rx, full) = ClientTx::new(usize::MAX);
        for _ in 0..CLIENT_QUEUE_LIMIT {
            tx.send(Out::Frame(vec![1]));
        }
        let not_yet = tokio::time::timeout(Duration::from_millis(20), full.notify.notified()).await;
        assert!(not_yet.is_err(), "a queue at its limit is not yet full");
        tx.send(Out::Frame(vec![2]));
        tokio::time::timeout(Duration::from_secs(1), full.notify.notified())
            .await
            .expect("told to close");
        assert!(matches!(rx.recv().await, Some(Out::Frame(f)) if f == [1]));
        // A client whose socket task is gone is not "full".
        drop(rx);
        tx.send(Out::Frame(vec![3]));
        let after = tokio::time::timeout(Duration::from_millis(20), full.notify.notified()).await;
        assert!(after.is_err());
    }

    /// A room's close that finds the queue full still reaches the client.
    #[tokio::test]
    async fn a_close_that_finds_the_queue_full_goes_around_it() {
        let (tx, _rx, full) = ClientTx::new(usize::MAX);
        for _ in 0..CLIENT_QUEUE_LIMIT {
            tx.send(Out::Frame(vec![1]));
        }
        tx.send(Out::Close(4404, "gone".into()));
        let got = tokio::time::timeout(Duration::from_secs(1), full.raised())
            .await
            .expect("raised");
        assert_eq!(got, (4404, "gone".to_string()));
    }

    /// A client holding more than its byte budget of frames is told to
    /// go, though far below the frame limit; one frame larger than the
    /// budget still reaches an empty queue (#472).
    #[tokio::test]
    async fn a_client_over_its_byte_budget_is_asked_to_close() {
        let (tx, mut rx, full) = ClientTx::new(1000);
        tx.send(Out::Frame(vec![0; 1500]));
        let alone = tokio::time::timeout(Duration::from_millis(20), full.notify.notified()).await;
        assert!(alone.is_err(), "one large frame fits an empty queue");
        // Taken by the socket task, the queue is empty again.
        let Some(Out::Frame(f)) = rx.recv().await else {
            panic!()
        };
        full.taken(&f);
        for _ in 0..3 {
            tx.send(Out::Frame(vec![0; 300]));
        }
        let under = tokio::time::timeout(Duration::from_millis(20), full.notify.notified()).await;
        assert!(under.is_err(), "900 bytes is within 1000");
        tx.send(Out::Frame(vec![0; 300]));
        let got = tokio::time::timeout(Duration::from_secs(1), full.raised())
            .await
            .expect("raised");
        assert_eq!(got, (1008, "fell 1000 bytes behind".to_string()));
        assert_eq!(full.queued.load(Ordering::SeqCst), 900);
    }

    /// A long write counts as hearing from the client, so a slow first
    /// sync isn't followed by an idle drop; a quick one doesn't.
    #[test]
    fn a_long_write_counts_as_hearing_from_the_client() {
        let ping = Duration::from_secs(30);
        let long_ago = Instant::now() - Duration::from_secs(120);
        assert!(heard_after_write(long_ago, Duration::from_secs(100), ping).elapsed() < ping);
        assert_eq!(
            heard_after_write(long_ago, Duration::from_millis(5), ping),
            long_ago
        );
    }

    /// A big frame gets time for a slow link; a small one the base timeout.
    #[test]
    fn write_timeout_grows_with_the_frame() {
        assert_eq!(write_timeout(100), WRITE_TIMEOUT);
        let sixty_mb = 60 * 1024 * 1024;
        assert!(write_timeout(sixty_mb) >= WRITE_TIMEOUT + Duration::from_secs(900));
    }
}

#[cfg(test)]
mod join_tests {
    use super::*;

    fn notebook_dir() -> (tempfile::TempDir, Arc<dyn Contents>) {
        let dir = tempfile::tempdir().unwrap();
        let nb = r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(dir.path().join("a.ipynb"), nb).unwrap();
        let contents: Arc<dyn Contents> =
            Arc::new(ganymede_contents::FsContents::new(dir.path(), Default::default()).unwrap());
        (dir, contents)
    }

    /// A command that reports once the room has loaded its file; a room
    /// whose file doesn't load closes, and the receiver errs.
    fn report_loaded() -> (Cmd, tokio::sync::oneshot::Receiver<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = Cmd::With(Box::new(move |_: &mut Room| {
            let _ = tx.send(());
            Default::default()
        }));
        (cmd, rx)
    }

    async fn loads(rx: tokio::sync::oneshot::Receiver<()>, what: &str) {
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap_or_else(|_| panic!("{what}"));
    }

    /// A join after a rename opens the room on the new path, not on the
    /// old one, which no longer loads.
    #[tokio::test]
    async fn a_join_after_a_rename_opens_the_new_path() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let (id, _) = collab.index("a.ipynb").await;
        std::fs::rename(dir.path().join("a.ipynb"), dir.path().join("b.ipynb")).unwrap();
        collab.moved("a.ipynb", "b.ipynb").await;

        let (cmd, rx) = report_loaded();
        let room_id = format!("json:notebook:{id}");
        send_to_room(&collab, &contents, &room_id, Some(&id), cmd)
            .await
            .unwrap();
        loads(rx, "the room loaded the renamed file").await;
    }

    /// A path with no file behind it gets no room and no file id (#560).
    #[tokio::test]
    async fn a_missing_notebook_gets_no_file_id() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let e = notebook_room_id(&collab, &contents, "typo.ipynb")
            .await
            .unwrap_err();
        assert_eq!(e.status(), 404);
        // Never indexed: the first index makes the id.
        assert!(collab.index("typo.ipynb").await.1);
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(notebook_room_id(&collab, &contents, "sub").await.is_err());
        assert!(collab.index("sub").await.1);
        let id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        assert_eq!(
            id,
            format!("json:notebook:{}", collab.index("a.ipynb").await.0)
        );
    }

    /// MCP resolves a path to a room id once and passes the id on to the
    /// hold and the call (#584): both reach the room after a rename.
    #[tokio::test]
    async fn a_room_id_reaches_its_notebook_after_a_rename() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let hold = hold_notebook(&collab, &contents, &room_id).await.unwrap();
        let read = |collab: Arc<Collab>, contents: Arc<dyn Contents>, room_id: String| async move {
            with_notebook(&collab, &contents, &room_id, |room| {
                (room.notebook_with_ids(), Default::default())
            })
            .await
        };
        let before = read(collab.clone(), contents.clone(), room_id.clone()).await;
        assert!(matches!(before, Some(Some(Ok(_)))), "loaded: {before:?}");

        let moving = collab.begin_move().await;
        std::fs::rename(dir.path().join("a.ipynb"), dir.path().join("b.ipynb")).unwrap();
        collab.moved("a.ipynb", "b.ipynb").await;
        drop(moving);
        let after = read(collab.clone(), contents.clone(), room_id.clone()).await;
        assert!(
            matches!(after, Some(Some(Ok(_)))),
            "after the rename: {after:?}"
        );
        assert!(hold.is_live());
    }

    /// Indexing a path while a rename is under way waits for it, so the
    /// old path, gone by then, gets no id (#574).
    #[tokio::test]
    async fn indexing_waits_for_a_rename() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let moving = collab.begin_move().await;
        let index = {
            let (collab, contents) = (collab.clone(), contents.clone());
            tokio::spawn(async move { notebook_room_id(&collab, &contents, "a.ipynb").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!index.is_finished(), "indexed during the rename");
        std::fs::rename(dir.path().join("a.ipynb"), dir.path().join("b.ipynb")).unwrap();
        collab.moved("a.ipynb", "b.ipynb").await;
        drop(moving);
        assert_eq!(index.await.unwrap().unwrap_err().status(), 404);
        assert!(collab.index("a.ipynb").await.1, "the old path got an id");
    }

    /// Indexing a path while a delete is under way waits for it, so the
    /// deleted path gets no id (#574).
    #[tokio::test]
    async fn indexing_waits_for_a_delete() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let deleting = collab.begin_move().await;
        let index = {
            let (collab, contents) = (collab.clone(), contents.clone());
            tokio::spawn(async move { notebook_room_id(&collab, &contents, "a.ipynb").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!index.is_finished(), "indexed during the delete");
        std::fs::remove_file(dir.path().join("a.ipynb")).unwrap();
        collab.removed("a.ipynb").await;
        drop(deleting);
        assert_eq!(index.await.unwrap().unwrap_err().status(), 404);
        assert!(
            collab.index("a.ipynb").await.1,
            "the deleted path got an id"
        );
    }

    /// `Collab::delete`, which `DELETE /api/contents` calls, waits for an
    /// index in progress and leaves the path without an id (#580).
    #[tokio::test]
    async fn delete_waits_for_indexing_and_drops_the_id() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        collab.index("a.ipynb").await;
        let indexing = collab.move_lock.read().await;
        let delete = {
            let (collab, contents) = (collab.clone(), contents.clone());
            tokio::spawn(async move { collab.delete(&contents, "a.ipynb").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!delete.is_finished(), "deleted during an index");
        assert!(dir.path().join("a.ipynb").exists());
        drop(indexing);
        delete.await.unwrap().unwrap();
        assert!(!dir.path().join("a.ipynb").exists());
        assert!(collab.index("a.ipynb").await.1, "the id stayed");
    }

    /// A delete or rename whose caller is dropped once it has started
    /// still finishes, ids included (#586).
    #[tokio::test]
    async fn a_dropped_caller_does_not_stop_a_delete_or_rename() {
        for rename in [false, true] {
            let (dir, contents) = notebook_dir();
            let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
            let (id, _) = collab.index("a.ipynb").await;
            let indexing = collab.move_lock.read().await;
            let caller = {
                let (collab, contents) = (collab.clone(), contents.clone());
                tokio::spawn(async move {
                    if rename {
                        collab
                            .rename(&contents, "a.ipynb", "b.ipynb")
                            .await
                            .map(drop)
                    } else {
                        collab.delete(&contents, "a.ipynb").await
                    }
                })
            };
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            caller.abort();
            let _ = caller.await;
            drop(indexing);
            // The detached task takes the lock for writing once it's free;
            // this read waits for it to finish.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(collab.move_lock.read().await);
            assert!(!dir.path().join("a.ipynb").exists(), "rename {rename}");
            if rename {
                assert_eq!(collab.path_of(&id).await.as_deref(), Some("b.ipynb"));
            } else {
                assert_eq!(collab.path_of(&id).await, None);
            }
        }
    }

    /// A websocket join or a server-side send while a rename is under way
    /// (on disk, ids not yet moved) waits for it, and opens the room on
    /// the new path (#522).
    #[tokio::test]
    async fn no_room_opens_while_a_rename_is_under_way() {
        for websocket in [true, false] {
            let (dir, contents) = notebook_dir();
            let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
            let (id, _) = collab.index("a.ipynb").await;
            let room_id = format!("json:notebook:{id}");

            let moving = collab.begin_move().await;
            std::fs::rename(dir.path().join("a.ipynb"), dir.path().join("b.ipynb")).unwrap();
            // A lookup now would still give the old path.
            assert_eq!(collab.path_of(&id).await.unwrap(), "a.ipynb");
            let (cmd, rx) = report_loaded();
            let join = {
                let (collab, contents, room_id, id) = (
                    collab.clone(),
                    contents.clone(),
                    room_id.clone(),
                    id.clone(),
                );
                tokio::spawn(async move {
                    if websocket {
                        // What `serve_socket` calls.
                        send_to_room(&collab, &contents, &room_id, Some(&id), cmd).await
                    } else {
                        send_to_notebook_room(&collab, &contents, &room_id, cmd).await
                    }
                })
            };
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(collab.open_rooms(), 0, "a room opened mid-rename");
            collab.moved("a.ipynb", "b.ipynb").await;
            drop(moving);

            join.await.unwrap().expect("joined");
            // Only `b.ipynb` exists, so a room that loaded opened on it.
            loads(rx, "the room loaded the renamed file").await;
        }
    }

    /// A rename that fails releases its lock, and rooms open again.
    #[tokio::test]
    async fn a_failed_rename_lets_rooms_open() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let (id, _) = collab.index("a.ipynb").await;
        drop(collab.begin_move().await);
        let (cmd, rx) = report_loaded();
        send_to_notebook_room(&collab, &contents, &format!("json:notebook:{id}"), cmd)
            .await
            .expect("joined");
        loads(rx, "the room loaded").await;
    }

    /// An open room takes a join with no path lookup: one whose id is
    /// gone (its file deleted) still reaches the room, which tells its
    /// clients the file was deleted, rather than "Unknown file id".
    /// `serve_socket` makes this same call, with no lookup before it.
    #[tokio::test]
    async fn an_open_room_takes_a_join_without_a_lookup() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let (id, _) = collab.index("a.ipynb").await;
        let room_id = format!("json:notebook:{id}");
        let (cmd, rx) = report_loaded();
        let open = send_to_notebook_room(&collab, &contents, &room_id, cmd)
            .await
            .unwrap();
        rx.await.unwrap();

        // The id goes, as a delete makes it go.
        collab.ids.removed("a.ipynb");
        assert_eq!(collab.path_of(&id).await, None);

        let (cmd, rx) = report_loaded();
        let joined = send_to_room(&collab, &contents, &room_id, Some(&id), cmd)
            .await
            .expect("the open room took the join");
        assert!(joined.same_channel(&open));
        rx.await.expect("the open room ran the command");
    }

    /// A room entry whose task has stopped is replaced by a fresh room,
    /// rather than the join failing as an unknown file id.
    #[tokio::test]
    async fn a_stopped_room_is_replaced() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let (id, _) = collab.index("a.ipynb").await;
        let room_id = format!("json:notebook:{id}");
        let (dead, rx) = mpsc::unbounded_channel();
        drop(rx);
        collab
            .rooms
            .lock()
            .unwrap()
            .insert(room_id.clone(), dead.clone());

        let (cmd, rx) = report_loaded();
        let joined = send_to_room(&collab, &contents, &room_id, Some(&id), cmd)
            .await
            .expect("a fresh room took the join");
        assert!(!joined.same_channel(&dead));
        loads(rx, "the fresh room loaded").await;
    }
}

#[cfg(test)]
mod trust_tests {
    use super::*;
    use ganymede_contents::{FsContents, FsOptions, Notary};

    const HTML: &str = r#"{"output_type":"display_data","metadata":{},"data":{"text/html":["<b>hi</b>"],"text/plain":["hi"]}}"#;

    fn contents(dir: &std::path::Path) -> (FsContents, Arc<Notary>) {
        let notary = Arc::new(Notary::ephemeral().unwrap());
        let opts = FsOptions {
            notary: Some(notary.clone()),
            ..Default::default()
        };
        (FsContents::new(dir, opts).unwrap(), notary)
    }

    fn signed(dir: &std::path::Path, notary: &Notary) -> bool {
        let text = std::fs::read_to_string(dir.join("a.ipynb")).unwrap();
        notary.check_signature(&serde_json::from_str(&text).unwrap())
    }

    /// Save after an edit to the Markdown cell, so the room is dirty.
    async fn edit_and_save(contents: &FsContents, room: &mut Room, disk: &mut Disk) {
        use ganymede_documents::yrs::{Array, Out, Text};
        room.edit(|txn, roots| {
            let Some(Out::YMap(cell)) = roots.cells.get(txn, 0) else {
                panic!("no first cell")
            };
            let Some(Out::YText(t)) = ganymede_documents::yrs::Map::get(&cell, txn, "source")
            else {
                panic!("no source")
            };
            t.insert(txn, 0, "x");
        });
        assert!(room.is_dirty());
        let status = save_room(contents, room, disk, &HashMap::new(), false).await;
        assert_eq!(status, SaveStatus::Success);
    }

    /// Give the code cell `outputs`, as a kernel run in the room would.
    fn run_cell(room: &mut Room, outputs: Vec<Value>) {
        use ganymede_documents::yrs::{Array, Out};
        room.edit(|txn, roots| {
            let Some(Out::YMap(cell)) = roots.cells.get(txn, 1) else {
                panic!("no code cell")
            };
            ganymede_documents::schema::set_outputs(txn, &cell, outputs);
        });
    }

    async fn open(contents: &FsContents) -> (Room, Disk) {
        let (nb, mut disk) = load(contents, "a.ipynb").await.ok().unwrap();
        let mut room = Room::notebook_room(&nb, "a.ipynb");
        room.saved(0, &disk.hash);
        disk.track(None);
        (room, disk)
    }

    /// The room's id for its code cell.
    fn code_id(room: &Room) -> String {
        let nb = room.notebook_with_ids().unwrap().unwrap();
        nb.cells[1].id().unwrap().to_string()
    }

    fn untrusted_nb(dir: &std::path::Path, minor: u32) {
        let ids = |id: &str| {
            if minor == 5 {
                format!(r#""id":"{id}","#)
            } else {
                String::new()
            }
        };
        let nb = format!(
            r#"{{"cells":[{{"cell_type":"markdown",{}"metadata":{{}},"source":["note"]}},
            {{"cell_type":"code",{}"execution_count":1,"metadata":{{}},"outputs":[{HTML}],"source":["show()"]}}],
            "metadata":{{}},"nbformat":4,"nbformat_minor":{minor}}}"#,
            ids("m"),
            ids("c")
        );
        std::fs::write(dir.join("a.ipynb"), nb).unwrap();
    }

    /// A notebook nobody signed stays unsigned when the room saves it,
    /// with or without cell ids, until its outputs come from a run here.
    #[tokio::test]
    async fn a_room_signs_only_outputs_it_ran() {
        for minor in [4, 5] {
            let dir = tempfile::tempdir().unwrap();
            untrusted_nb(dir.path(), minor);
            let (contents, notary) = contents(dir.path());
            let (mut room, mut disk) = open(&contents).await;

            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(!signed(dir.path(), &notary), "4.{minor}: untrusted outputs");

            run_cell(
                &mut room,
                vec![serde_json::from_str(&HTML.replace("hi", "again")).unwrap()],
            );
            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(
                signed(dir.path(), &notary),
                "4.{minor}: outputs from a run here"
            );

            // Reopened, the signed file is trusted and stays signed.
            let (mut room, mut disk) = open(&contents).await;
            assert!(disk.untrusted.is_empty() && disk.read_untrusted.is_none());
            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(signed(dir.path(), &notary));
        }
    }

    /// Outputs another program writes into the file unsigned stay
    /// untrusted after the room merges them in.
    #[tokio::test]
    async fn outputs_merged_from_an_unsigned_file_stay_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let nb = r#"{"cells":[{"cell_type":"markdown","id":"m","metadata":{},"source":["note"]},
            {"cell_type":"code","id":"c","execution_count":null,"metadata":{},"outputs":[],"source":["show()"]}],
            "metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(dir.path().join("a.ipynb"), nb).unwrap();
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(
            signed(dir.path(), &notary),
            "no outputs: nothing to distrust"
        );

        let text = std::fs::read_to_string(dir.path().join("a.ipynb")).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["cells"][1]["outputs"] = json!([serde_json::from_str::<Value>(HTML).unwrap()]);
        std::fs::write(dir.path().join("a.ipynb"), v.to_string()).unwrap();
        reload(&contents, &mut room, &mut disk, &HashMap::new()).await;
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(!signed(dir.path(), &notary));
    }

    /// A run here that writes the same outputs the unsigned file had
    /// makes them trusted: rerunning an old journal repairs it.
    #[tokio::test]
    async fn an_identical_rerun_here_is_trusted() {
        for minor in [4, 5] {
            let dir = tempfile::tempdir().unwrap();
            untrusted_nb(dir.path(), minor);
            let (contents, notary) = contents(dir.path());
            let (mut room, mut disk) = open(&contents).await;
            run_cell(&mut room, vec![serde_json::from_str(HTML).unwrap()]);
            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(!signed(dir.path(), &notary), "same outputs, no run here");
            disk.ran(&room, &code_id(&room));
            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(signed(dir.path(), &notary), "4.{minor}: rerun here");
        }
    }

    /// An identical rerun is trusted even when its outputs hold numbers
    /// Yjs can't keep exactly (review of #598).
    #[tokio::test]
    async fn an_identical_rerun_with_big_numbers_is_trusted() {
        let big = r#"{"output_type":"display_data","data":{"application/json":{"n":9007199254740993,"x":10000000000.0}},"metadata":{}}"#;
        for minor in [4, 5] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a.ipynb");
            untrusted_nb(dir.path(), minor);
            let text = std::fs::read_to_string(&path).unwrap().replace(HTML, big);
            std::fs::write(&path, text).unwrap();
            let (contents, notary) = contents(dir.path());
            let (mut room, mut disk) = open(&contents).await;
            run_cell(&mut room, vec![serde_json::from_str(big).unwrap()]);
            disk.ran(&room, &code_id(&room));
            assert!(disk.untrusted.is_empty(), "4.{minor}");
            edit_and_save(&contents, &mut room, &mut disk).await;
            assert!(signed(dir.path(), &notary), "4.{minor}");
        }
    }

    /// Trusting the file while the room is open (`ganymede trust`) holds:
    /// the room's next save signs it.
    #[tokio::test]
    async fn trusting_the_file_with_the_room_open_holds() {
        let dir = tempfile::tempdir().unwrap();
        untrusted_nb(dir.path(), 5);
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        let text = std::fs::read_to_string(dir.path().join("a.ipynb")).unwrap();
        notary.sign(&serde_json::from_str(&text).unwrap()).unwrap();
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(signed(dir.path(), &notary));
        assert!(disk.untrusted.is_empty());
    }

    /// Clearing one of two untrusted outputs leaves the cell untrusted.
    #[tokio::test]
    async fn part_of_an_untrusted_output_list_stays_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let two = format!("{HTML},{}", HTML.replace("hi", "two"));
        let nb = format!(
            r#"{{"cells":[{{"cell_type":"markdown","id":"m","metadata":{{}},"source":["note"]}},
            {{"cell_type":"code","id":"c","execution_count":1,"metadata":{{}},"outputs":[{two}],"source":["show()"]}}],
            "metadata":{{}},"nbformat":4,"nbformat_minor":5}}"#
        );
        std::fs::write(dir.path().join("a.ipynb"), nb).unwrap();
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        run_cell(&mut room, vec![serde_json::from_str(HTML).unwrap()]);
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(!signed(dir.path(), &notary));
    }

    /// Outputs whose objects have several keys stay untrusted, though the
    /// room gives the keys back in another order (review of #598).
    #[tokio::test]
    async fn untrusted_outputs_match_whatever_the_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let meta: String = (1..=8)
            .map(|i| format!(r#""k{i}":{i}"#))
            .collect::<Vec<_>>()
            .join(",");
        let out = format!(
            r#"{{"output_type":"display_data","metadata":{{{meta}}},"data":{{"text/html":"<script>x</script>","text/plain":"x","application/json":{{"a":1.5,"b":[1,2],"c":{{"d":1,"e":2}}}}}}}}"#
        );
        let nb = format!(
            r#"{{"cells":[{{"cell_type":"markdown","id":"m","metadata":{{}},"source":["note"]}},
            {{"cell_type":"code","id":"c","execution_count":1,"metadata":{{}},"outputs":[{out}],"source":["show()"]}}],
            "metadata":{{}},"nbformat":4,"nbformat_minor":5}}"#
        );
        std::fs::write(dir.path().join("a.ipynb"), nb).unwrap();
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(!signed(dir.path(), &notary));
    }

    /// A cell cut and pasted under a new id keeps its untrusted outputs
    /// untrusted.
    #[tokio::test]
    async fn a_cell_under_a_new_id_stays_untrusted() {
        use ganymede_documents::yrs::{Array, Map, Out};
        let dir = tempfile::tempdir().unwrap();
        untrusted_nb(dir.path(), 5);
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        room.edit(|txn, roots| {
            let Some(Out::YMap(cell)) = roots.cells.get(txn, 1) else {
                panic!("no code cell")
            };
            cell.insert(txn, "id", "pasted");
        });
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(!signed(dir.path(), &notary));
    }

    /// Outputs the room ran itself stay trusted when another program
    /// writes the file unsigned with them in it.
    #[tokio::test]
    async fn an_unsigned_write_keeps_the_rooms_own_outputs_trusted() {
        let dir = tempfile::tempdir().unwrap();
        untrusted_nb(dir.path(), 5);
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        run_cell(&mut room, vec![serde_json::from_str(HTML).unwrap()]);
        disk.ran(&room, &code_id(&room));
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(signed(dir.path(), &notary));
        // Another program edits the note and writes the file unsigned.
        let text = std::fs::read_to_string(dir.path().join("a.ipynb")).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["cells"][0]["source"] = json!("edited elsewhere");
        std::fs::write(dir.path().join("a.ipynb"), v.to_string()).unwrap();
        assert!(!signed(dir.path(), &notary));
        reload(&contents, &mut room, &mut disk, &HashMap::new()).await;
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(signed(dir.path(), &notary));
    }

    /// Trust given between two saves holds, however soon after the
    /// last one.
    #[tokio::test]
    async fn trust_between_saves_holds() {
        let dir = tempfile::tempdir().unwrap();
        untrusted_nb(dir.path(), 5);
        let (contents, notary) = contents(dir.path());
        let (mut room, mut disk) = open(&contents).await;
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(!signed(dir.path(), &notary));
        let text = std::fs::read_to_string(dir.path().join("a.ipynb")).unwrap();
        notary.sign(&serde_json::from_str(&text).unwrap()).unwrap();
        edit_and_save(&contents, &mut room, &mut disk).await;
        assert!(signed(dir.path(), &notary));
    }
}
