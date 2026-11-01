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
//! Each room is a task that owns a [`Room`] and the file: it loads the
//! notebook through [`Contents`], autosaves a second after the last edit
//! with an `If-Match` on the hash it last saw, and polls the file every
//! second, reloading the room when the file changed and the change is not
//! its own save. A save refused because the file changed reloads too, as
//! `jupyter-collaboration` does: the disk wins. A room with no clients is
//! saved and dropped after a minute.
//!
//! File ids are made per server run and kept in memory; they don't follow
//! renames yet.

use crate::routes::{origin_allowed, HttpError, LenientJson};
use crate::AppState;
use axum::extract::ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ganymede_contents::{ContentType, Contents, GetOptions, IfMatch, SaveRequest};
use ganymede_documents::{ConnId, Room, SaveStatus, Target};
use ganymede_nbformat::Notebook;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long after the last edit a room saves.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// How often a room checks its file for changes made elsewhere.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long a room with no clients is kept, in case one reconnects.
const CLEANUP_DELAY: Duration = Duration::from_secs(60);

/// Rooms and file ids for one server.
pub struct Collab {
    /// Changes every server run; clients holding another one are asked to
    /// reload, as `jupyter-collaboration` does after a restart.
    session_id: String,
    ids: Mutex<FileIds>,
    rooms: Mutex<HashMap<String, mpsc::UnboundedSender<Cmd>>>,
    next_conn: AtomicU64,
}

impl Default for Collab {
    fn default() -> Self {
        Collab {
            session_id: uuid::Uuid::new_v4().to_string(),
            ids: Mutex::default(),
            rooms: Mutex::default(),
            next_conn: AtomicU64::new(1),
        }
    }
}

#[derive(Default)]
struct FileIds {
    by_path: HashMap<String, String>,
    by_id: HashMap<String, String>,
}

enum Cmd {
    Join(ConnId, mpsc::UnboundedSender<Out>),
    Frame(ConnId, Vec<u8>),
    Leave(ConnId),
}

enum Out {
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
    let (id, new) = {
        let mut ids = state.collab.ids.lock().expect("file ids");
        match ids.by_path.get(&path) {
            Some(id) => (id.clone(), false),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                ids.by_path.insert(path.clone(), id.clone());
                ids.by_id.insert(id.clone(), path.clone());
                (id, true)
            }
        }
    };
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
        let path = collab
            .ids
            .lock()
            .expect("file ids")
            .by_id
            .get(file_id)
            .cloned();
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
        let tx = rooms
            .entry(room_id.clone())
            .or_insert_with(|| {
                let (tx, rx) = mpsc::unbounded_channel();
                tokio::spawn(run_room(
                    collab.clone(),
                    state.contents.clone(),
                    room_id.clone(),
                    doc.clone(),
                    rx,
                ));
                tx
            })
            .clone();
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

async fn close(socket: &mut WebSocket, code: u16, reason: String) {
    let _ = socket
        .send(WsMessage::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// A room's file: what was last read or written, to tell our own writes
/// from someone else's.
struct Disk {
    path: String,
    hash: String,
    last_modified: String,
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
        nb,
        Disk {
            path: path.to_string(),
            hash: model.hash.unwrap_or_default(),
            last_modified: model.last_modified,
        },
    ))
}

async fn run_room(
    collab: Arc<Collab>,
    contents: Arc<dyn Contents>,
    room_id: String,
    path: Option<String>,
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
                    if let Cmd::Join(_, out) = cmd {
                        let _ = out.send(Out::Close(4500, format!("Error initializing: {path}.")));
                    }
                }
                return;
            }
        },
    };
    tracing::info!(room = %room_id, path = ?path, "room opened");

    let mut last_edit: Option<Instant> = None;
    let mut empty_since: Option<Instant> = None;
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
                }
            }
            _ = tick.tick() => {
                let Some(d) = disk.as_mut() else {
                    if clients.is_empty() && retire(&collab, &room_id, &mut rx) { break }
                    continue;
                };
                if last_edit.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
                    save_room(contents.as_ref(), &mut room, d, &clients, false).await;
                    last_edit = None;
                } else if last_edit.is_none() && last_poll.elapsed() >= POLL_INTERVAL {
                    last_poll = Instant::now();
                    poll(contents.as_ref(), &mut room, d, &clients).await;
                }
                if empty_since.is_some_and(|t| t.elapsed() >= CLEANUP_DELAY) {
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

/// Write the room to its file, if it changed or `force`.
async fn save_room(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
    force: bool,
) -> SaveStatus {
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
            let fx = room.saved(generation, &disk.hash);
            dispatch(clients, fx.send);
            tracing::debug!(path = %disk.path, "room saved");
            SaveStatus::Success
        }
        Err(e) if e.status() == 412 => {
            tracing::info!(path = %disk.path, "file changed on disk; reloading the room instead of saving");
            reload(contents, room, disk, clients).await;
            SaveStatus::Failed
        }
        Err(e) => {
            tracing::warn!(path = %disk.path, "room save failed: {e}");
            SaveStatus::Failed
        }
    }
}

/// Reload when the file changed and it was not our write.
async fn poll(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
) {
    let Ok(model) = contents.get(&disk.path, GetOptions::default()).await else {
        return; // gone or unreadable: keep the room as it is
    };
    if model.last_modified == disk.last_modified {
        return;
    }
    reload(contents, room, disk, clients).await;
}

async fn reload(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, mpsc::UnboundedSender<Out>>,
) {
    match load(contents, &disk.path).await {
        Ok((nb, fresh)) => {
            let ours = fresh.hash == disk.hash;
            disk.last_modified = fresh.last_modified;
            if ours {
                return; // touched, not changed (or our own write)
            }
            disk.hash = fresh.hash;
            tracing::info!(path = %disk.path, "file changed on disk; reloading the room");
            let fx = room.reload(&nb, &disk.hash);
            dispatch(clients, fx.send);
        }
        Err(e) => tracing::warn!(path = %disk.path, "cannot reload: {e}"),
    }
}
