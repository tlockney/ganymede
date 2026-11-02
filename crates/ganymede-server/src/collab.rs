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
//! second, and polls every ten seconds besides; where there are no file
//! events, it polls every second. A change whose hash is the room's own
//! last save is ignored. Any other change is merged into the room (see
//! [`ganymede_documents::merge`]): a clean room just reloads; a room with
//! unsaved edits keeps them, takes the disk's changes to other cells, and
//! saves the merge. Where both changed the same cell, the disk wins that
//! cell. A room with no clients is saved and dropped after a minute.
//!
//! File ids come from [`FileIds`]: kept in a database when the server has
//! one, so they survive restarts, and moved by renames through the
//! contents API, which also move open rooms.

use crate::routes::{origin_allowed, HttpError, LenientJson};
use crate::watch::{Watch, Watcher};
use crate::AppState;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ganymede_contents::{ContentType, Contents, FileIds, GetOptions, IfMatch, SaveRequest};
use ganymede_documents::{ConnId, Room, SaveStatus, Target};
use ganymede_nbformat::Notebook;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
/// How long a room with no clients is kept, in case one reconnects.
const CLEANUP_DELAY: Duration = Duration::from_secs(60);

/// Rooms and file ids for one server.
pub struct Collab {
    /// Changes every server run; clients holding another one are asked to
    /// reload, as `jupyter-collaboration` does after a restart.
    session_id: String,
    ids: FileIds,
    /// The contents root on disk, for watching files; `None` when the
    /// contents are not a local directory, and rooms poll.
    root: Option<PathBuf>,
    watcher: Arc<Watcher>,
    rooms: Mutex<HashMap<String, mpsc::UnboundedSender<Cmd>>>,
    next_conn: AtomicU64,
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
            ids,
            root,
            watcher: Watcher::new(),
            rooms: Mutex::default(),
            next_conn: AtomicU64::new(1),
            requests: Default::default(),
        }
    }

    /// `old` (a file or directory) was renamed to `new` through the
    /// contents API: ids follow, and open rooms take the new path.
    pub fn moved(&self, old: &str, new: &str) {
        self.ids.moved(old, new);
        for room in self.rooms.lock().expect("rooms").values() {
            let _ = room.send(Cmd::Moved(old.to_string(), new.to_string()));
        }
    }

    /// The contents root on disk, when the contents are a local directory.
    pub(crate) fn root(&self) -> Option<&std::path::Path> {
        self.root.as_deref()
    }

    /// `path` was deleted through the contents API.
    pub fn removed(&self, path: &str) {
        self.ids.removed(path);
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
    Join(ConnId, mpsc::UnboundedSender<Out>),
    Frame(ConnId, Vec<u8>),
    Leave(ConnId),
    /// A rename through the contents API: old and new path.
    Moved(String, String),
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
    let path = path.trim_matches('/').to_string();
    state
        .contents
        .get(&path, GetOptions::default())
        .await
        .map_err(|e| match e.status() {
            404 => HttpError(404, format!("File {path:?} does not exist")),
            _ => HttpError::from(e),
        })?;
    let (id, new) = state.collab.ids.index(&path);
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
    state.contents.get(&path, GetOptions::default()).await?;
    let (id, _) = state.collab.ids.index(&path);
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
    let id =
        state.collab.ids.get(&path).ok_or_else(|| {
            HttpError(404, format!("The ID for file, {path}, could not be found."))
        })?;
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
        state.collab.ids.path(&id).ok_or_else(|| {
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
    ws.max_message_size(1 << 30)
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
    let doc = if let [format, kind, file_id] = parts[..] {
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
        let path = collab.ids.path(file_id);
        let Some(path) = path else {
            close(&mut socket, 4404, format!("Unknown file id {file_id}")).await;
            return;
        };
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
        Some(path)
    } else {
        None
    };

    let conn = collab.next_conn.fetch_add(1, Ordering::Relaxed);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let room = {
        let mut rooms = collab.rooms.lock().expect("rooms");
        let tx = open_room(&collab, &mut rooms, &state.contents, &room_id, doc);
        let _ = tx.send(Cmd::Join(conn, out_tx));
        tx
    };
    tracing::debug!(room = %room_id, conn, "collaboration client connected");

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(WsMessage::Binary(b))) => {
                    if room.send(Cmd::Frame(conn, b.to_vec())).is_err() { break }
                }
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Text(_))) => {}
                Some(Ok(WsMessage::Close(_))) | None => break,
                Some(Err(e)) => { tracing::debug!("collaboration websocket error: {e}"); break }
            },
            out = out_rx.recv() => match out {
                Some(Out::Frame(f)) => {
                    if socket.send(WsMessage::Binary(f.into())).await.is_err() { break }
                }
                Some(Out::Close(code, reason)) => { close(&mut socket, code, reason).await; break }
                None => break,
            },
        }
    }
    let _ = room.send(Cmd::Leave(conn));
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
/// room id names no known notebook.
pub(crate) fn send_to_notebook_room(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    cmd: Cmd,
) -> Option<()> {
    let parts: Vec<&str> = room_id.splitn(3, ':').collect();
    let ["json", "notebook", file_id] = parts[..] else {
        return None;
    };
    let mut rooms = collab.rooms.lock().expect("rooms");
    let tx = match rooms.get(room_id) {
        Some(tx) => tx.clone(),
        None => {
            let path = collab.ids.path(file_id)?;
            open_room(collab, &mut rooms, contents, room_id, Some(path))
        }
    };
    tx.send(cmd).ok()
}

/// The room id of the notebook at `path`.
pub(crate) fn notebook_room_id(collab: &Collab, path: &str) -> String {
    format!("json:notebook:{}", collab.ids.index(path).0)
}

/// Run `f` on the room of the notebook at `path`, opening it if needed.
/// `None` when the room can't be opened (the file is gone or not a
/// notebook).
pub(crate) async fn with_notebook<R: Send + 'static>(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    path: &str,
    f: impl FnOnce(&mut Room) -> (R, ganymede_documents::Effects) + Send + 'static,
) -> Option<R> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let cmd = Cmd::With(Box::new(move |room: &mut Room| {
        let (r, fx) = f(room);
        let _ = tx.send(r);
        fx
    }));
    send_to_notebook_room(collab, contents, &notebook_room_id(collab, path), cmd)?;
    rx.await.ok()
}

async fn close(socket: &mut WebSocket, code: u16, reason: String) {
    let _ = socket
        .send(WsMessage::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// A room's file: what was last read or written, to tell our own writes
/// from someone else's and to merge theirs.
struct Disk {
    path: String,
    hash: String,
    last_modified: String,
    /// The notebook as last read or written: the base of a merge.
    base: Notebook,
}

async fn load(contents: &dyn Contents, path: &str) -> Result<(Notebook, Disk), String> {
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
        .map_err(|e| e.to_string())?;
    let nb =
        Notebook::from_value(model.content.unwrap_or(Value::Null)).map_err(|e| e.to_string())?;
    Ok((
        nb.clone(),
        Disk {
            path: path.to_string(),
            hash: model.hash.unwrap_or_default(),
            last_modified: model.last_modified,
            base: nb,
        },
    ))
}

async fn run_room(
    collab: Arc<Collab>,
    contents: Arc<dyn Contents>,
    room_id: String,
    path: Option<String>,
    me: mpsc::UnboundedSender<Cmd>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let mut clients: HashMap<ConnId, mpsc::UnboundedSender<Out>> = HashMap::new();
    let (mut room, mut disk) = match &path {
        None => (Room::transient(), None),
        Some(path) => match load(contents.as_ref(), path).await {
            Ok((nb, disk)) => {
                let mut room = Room::notebook_room(&nb, path);
                room.saved(0, &disk.hash);
                (room, Some(disk))
            }
            Err(e) => {
                tracing::warn!(room = %room_id, "cannot load {path}: {e}");
                let mut rooms = collab.rooms.lock().expect("rooms");
                rooms.remove(&room_id);
                rx.close();
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Join(_, out) => {
                            let _ =
                                out.send(Out::Close(4500, format!("Error initializing: {path}.")));
                        }
                        Cmd::Exec(cmd) => cmd.unavailable(&format!("cannot load {path}: {e}")),
                        _ => {}
                    }
                }
                return;
            }
        },
    };
    let mut watch = path.as_deref().and_then(|p| collab.watch(p));
    tracing::info!(room = %room_id, path = ?path, watched = watch.is_some(), "room opened");

    let mut last_edit: Option<Instant> = None;
    // A room opened for execution, with no client, retires like one whose
    // clients left.
    let mut empty_since: Option<Instant> = Some(Instant::now());
    let mut exec = crate::execute::RoomExec::new(me);
    // When file events last arrived and are not yet looked at.
    let mut disk_event: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut last_poll = Instant::now();
    loop {
        tokio::select! {
            cmd = rx.recv() => {
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
                                if status == SaveStatus::Success { last_edit = None }
                                if let Some(out) = clients.get(&conn) {
                                    let _ = out.send(Out::Frame(Room::save_reply(id, status)));
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
                    }
                    Cmd::With(f) => {
                        let fx = f(&mut room);
                        if fx.changed { last_edit = Some(Instant::now()) }
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Exec(cmd) => {
                        let fx = exec.handle(&mut room, cmd);
                        if fx.changed { last_edit = Some(Instant::now()) }
                        dispatch(&clients, fx.send);
                    }
                    Cmd::Flush(done) => {
                        if let Some(d) = disk.as_mut() {
                            if room.is_dirty() {
                                save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                            }
                        }
                        last_edit = None;
                        let _ = done.send(());
                    }
                }
            }
            Some(()) = changed(&mut watch) => {
                disk_event = Some(Instant::now());
            }
            _ = tick.tick() => {
                let Some(d) = disk.as_mut() else {
                    if clients.is_empty() && retire(&collab, &room_id, &mut rx) { break }
                    continue;
                };
                let poll_every = if watch.is_some() { WATCHED_POLL_INTERVAL } else { POLL_INTERVAL };
                if disk_event.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
                    disk_event = None;
                    last_poll = Instant::now();
                    if reload(contents.as_ref(), &mut room, d, &clients).await {
                        last_edit = Some(Instant::now());
                    }
                } else if last_poll.elapsed() >= poll_every && disk_event.is_none() {
                    last_poll = Instant::now();
                    if poll(contents.as_ref(), &mut room, d, &clients).await {
                        last_edit = Some(Instant::now());
                    }
                }
                if last_edit.is_some_and(|t| t.elapsed() >= SAVE_DELAY) && disk_event.is_none() {
                    save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                    last_edit = None;
                }
                if empty_since.is_some_and(|t| t.elapsed() >= CLEANUP_DELAY) && exec.is_idle() {
                    if room.is_dirty() {
                        save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                    }
                    if retire(&collab, &room_id, &mut rx) { break }
                    empty_since = Some(Instant::now());
                }
            }
        }
    }
    tracing::info!(room = %room_id, "room closed");
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
    if path == old {
        return Some(new.to_string());
    }
    let rest = path.strip_prefix(old)?.strip_prefix('/')?;
    Some(format!("{new}/{rest}"))
}

/// Remove the room from the map, unless a client is on its way in.
fn retire(collab: &Collab, room_id: &str, rx: &mut mpsc::UnboundedReceiver<Cmd>) -> bool {
    let mut rooms = collab.rooms.lock().expect("rooms");
    if !rx.is_empty() {
        return false;
    }
    rooms.remove(room_id);
    true
}

fn dispatch(clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>, send: Vec<(Target, Vec<u8>)>) {
    for (target, frame) in send {
        for (conn, out) in clients {
            let to = match target {
                Target::One(c) => *conn == c,
                Target::Others(c) => *conn != c,
                Target::All => true,
            };
            if to {
                let _ = out.send(Out::Frame(frame.clone()));
            }
        }
    }
}

/// Write the room to its file, if it changed or `force`. When the file
/// changed since it was last read, merge it in first and save the merge.
async fn save_room(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
    force: bool,
) -> SaveStatus {
    // Each round either saves or merges a newer file; give up if another
    // program keeps writing.
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
        let content = serde_json::to_value(&nb).expect("a notebook serializes");
        let request =
            SaveRequest::notebook(content).with_if_match(IfMatch::Hashes(vec![disk.hash.clone()]));
        match contents.save(&disk.path, request).await {
            Ok(model) => {
                disk.hash = model.hash.unwrap_or_default();
                disk.last_modified = model.last_modified;
                disk.base = nb;
                let fx = room.saved(generation, &disk.hash);
                dispatch(clients, fx.send);
                tracing::debug!(path = %disk.path, "room saved");
                return SaveStatus::Success;
            }
            Err(e) if e.status() == 412 => {
                tracing::info!(path = %disk.path, "file changed on disk; merging before saving");
                reload(contents, room, disk, clients).await;
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
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
) -> bool {
    let Ok(model) = contents.get(&disk.path, GetOptions::default()).await else {
        return false; // gone or unreadable: keep the room as it is
    };
    if model.last_modified == disk.last_modified {
        return false;
    }
    reload(contents, room, disk, clients).await
}

/// Read the file and merge it into the room, unless it is what we last
/// read or wrote. True when the room holds a merge that needs saving.
async fn reload(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
) -> bool {
    match load(contents, &disk.path).await {
        Ok((nb, fresh)) => {
            disk.last_modified = fresh.last_modified;
            if fresh.hash == disk.hash {
                return false; // touched, not changed (or our own write)
            }
            disk.hash = fresh.hash;
            let (fx, conflicts) = room.merge_disk(&disk.base, &nb, &disk.hash);
            disk.base = nb;
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
            room.is_dirty()
        }
        Err(e) => {
            tracing::warn!(path = %disk.path, "cannot reload: {e}");
            false
        }
    }
}
