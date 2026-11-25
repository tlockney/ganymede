//! `/api/collaboration`: document rooms over websockets, compatible with
//! `jupyter-collaboration` (`jupyter_server_ydoc` 3), so JupyterLab with its
//! prebuilt `@jupyter/docprovider-extension` edits notebooks through
//! Ganymede.
//!
//! - `PUT /api/collaboration/session/{path}` with `{"format", "type"}`
//!   answers `{"format", "type", "fileId", "sessionId"}` (201 the first
//!   time a path is seen, 200 after).
//! - `GET /api/collaboration/recovery?path=`, `GET` and `DELETE
//!   /api/collaboration/recovery/{id}` list, return and discard recovery
//!   copies.
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
//! cell, and the room's version of it is kept in a recovery copy (#945).
//! A room with no clients is saved and dropped after a minute, and never
//! while it holds edits the file doesn't have, unless a recovery copy of
//! them is on disk (`recovery.rs`) or they are not a notebook, which no
//! copy can hold. A delete through the contents API closes the rooms on
//! the deleted file.
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

use crate::path_locks::Scope;
use crate::recovery;
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
use ganymede_contents::{
    normalize_api_path, ContentType, Contents, FileIds, GetOptions, IfMatch, SaveRequest,
};
use ganymede_documents::{ConnId, Room, SaveStatus, Target};
use ganymede_nbformat::Notebook;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
// The runtime's clock, so a test can pause it and let minutes of backoff pass.
use tokio::sync::mpsc;
use tokio::time::Instant;

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
    /// and by [`file_gone`], from the disk change until the ids follow
    /// ([`Collab::begin_move`]). Held for reading by a room opening across
    /// its path lookup, by [`Collab::index_existing`], by a grant read, and
    /// by a copy into a directory or a new untitled file for the whole
    /// create ([`Collab::begin_create`]). So no room opens and no id is
    /// made for a path that is moving away or going (#522, #574), and no
    /// rename or delete lands during a create, which waits for them in
    /// turn. Readers don't wait for each other, but the lock is fair: a
    /// reader arriving while a writer waits queues behind it.
    move_lock: tokio::sync::RwLock<()>,
    /// What keeps a lookup from a path a create is about to make. A
    /// create claims the direct children of its directory here (a new
    /// folder: its directory's whole subtree), from before the file is
    /// made until [`Collab::forget_new_file`] is done;
    /// a grant read, an [`Collab::index_existing`] and a room opening
    /// register the path they read, which waits while a create claims its
    /// directory, and a create waits for the lookups registered directly in
    /// its directory. A lookup of any other path, the root's included, is
    /// never held up. Taken after `move_lock`, and
    /// nothing takes `move_lock` while holding it.
    lookups: Arc<crate::path_locks::PathLocks>,
    next_conn: AtomicU64,
    /// How long a room with no clients is kept: [`CLEANUP_DELAY`].
    cleanup_delay: Duration,
    /// How often sockets are pinged, and how long one may stay silent:
    /// [`PING_INTERVAL`] and [`IDLE_TIMEOUT`].
    heartbeat: (Duration, Duration),
    /// Which rooms own their files on this machine (#272).
    locks: crate::room_lock::Locks,
    /// Where a room keeps a copy of edits it would otherwise lose
    /// (`recovery.rs`); `None` in tests and embedding apps that set
    /// none, where such edits are only logged.
    recovery: Option<recovery::Store>,
    /// Request ids the execute endpoint has queued.
    pub(crate) requests: crate::execute::Requests,
    /// Stamps of files for host grants, and the saves in flight
    /// ([`crate::stamping`]).
    pub(crate) stamps: Arc<crate::stamping::Stamps>,
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
            stamps: Arc::new(crate::stamping::Stamps::new(root.clone())),
            root,
            watcher: Watcher::new(),
            rooms: Mutex::default(),
            moves: AtomicU64::new(0),
            move_lock: tokio::sync::RwLock::new(()),
            lookups: Arc::default(),
            next_conn: AtomicU64::new(1),
            cleanup_delay: CLEANUP_DELAY,
            heartbeat: (PING_INTERVAL, IDLE_TIMEOUT),
            locks: Default::default(),
            recovery: None,
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

    /// Rooms keep a copy of the edits they would lose in this root's part
    /// of `dir` (see `recovery.rs`), and never retire holding edits they
    /// couldn't keep a copy of. Without this they are only logged, and so
    /// they are, after a warning, where `dir` can't be made: rooms would
    /// wait forever for a copy.
    pub fn with_recovery_dir(mut self, dir: PathBuf) -> Self {
        match recovery::make_dir(&dir) {
            Ok(()) => {
                self.recovery = Some(recovery::Store::new(&dir, self.root.as_deref()));
            }
            Err(e) => tracing::warn!(
                dir = %dir.display(),
                "cannot make the recovery dir; running without recovery copies: {e}"
            ),
        }
        self
    }

    /// Reports each write the server makes of a file under the root to
    /// `observer` ([`crate::write_observer`]). Call before the contents
    /// are wrapped by [`Collab::stamping`].
    pub fn with_write_observer(
        mut self,
        observer: Arc<crate::write_observer::WriteObserver>,
    ) -> Self {
        self.stamps =
            Arc::new(crate::stamping::Stamps::new(self.root.clone()).with_observer(observer));
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

    /// The ids of the notebook rooms open now.
    pub(crate) fn notebook_rooms(&self) -> Vec<String> {
        self.rooms
            .lock()
            .expect("rooms")
            .iter()
            .filter(|(id, tx)| id.starts_with("json:notebook:") && !tx.is_closed())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The notebook rooms open now, by path. Rooms for awareness only are
    /// left out. A room that doesn't answer within two seconds (busy with
    /// a slow save) is left out too, rather than holding up the list.
    pub(crate) async fn list_rooms(&self) -> Vec<ListedRoom> {
        let open: Vec<(String, mpsc::UnboundedSender<Cmd>)> = self
            .rooms
            .lock()
            .expect("rooms")
            .iter()
            .filter(|(id, tx)| id.starts_with("json:notebook:") && !tx.is_closed())
            .map(|(id, tx)| (id.clone(), tx.clone()))
            .collect();
        let asked = open.into_iter().map(|(id, tx)| async move {
            let (reply, rx) = tokio::sync::oneshot::channel();
            tx.send(Cmd::Info(reply)).ok()?;
            let info = tokio::time::timeout(Duration::from_secs(2), rx)
                .await
                .ok()?
                .ok()?;
            let file_id = id.strip_prefix("json:notebook:")?.to_string();
            Some(ListedRoom {
                path: info.path,
                file_id,
                clients: info
                    .clients
                    .into_iter()
                    .map(|c| RoomClient {
                        name: c.name,
                        agent: c.agent,
                    })
                    .collect(),
                last_change: info.last_change.map(|(at, by, person)| RoomChange {
                    at: jiff::Timestamp::try_from(at)
                        .map(|t| t.strftime("%Y-%m-%dT%H:%M:%S.%3fZ").to_string())
                        .unwrap_or_default(),
                    by,
                    person,
                }),
            })
        });
        let mut rooms: Vec<ListedRoom> = futures::future::join_all(asked)
            .await
            .into_iter()
            .flatten()
            .collect();
        rooms.sort_by(|a, b| (&a.path, &a.file_id).cmp(&(&b.path, &b.file_id)));
        rooms
    }

    /// Recovery copy `id`, of notebook `path`, is deleted: open rooms that
    /// name it in `state.recovery` name the newest copy of `path` that is
    /// left, or none. With no `path` (its meta couldn't be read) they name
    /// none.
    async fn forget_recovery(&self, id: &str, path: Option<&str>) {
        let left = match (&self.recovery, path) {
            (Some(store), Some(path)) => {
                let (store, p) = (store.clone(), path.to_string());
                match blocking(move || recovery::list(&store, &p)).await {
                    Ok(copies) => copies.first().map(recovery::Meta::summary),
                    Err(e) => {
                        tracing::warn!(id, path, "cannot list the recovery copies left: {e}");
                        None
                    }
                }
            }
            _ => None,
        };
        let rooms: Vec<mpsc::UnboundedSender<Cmd>> = self
            .rooms
            .lock()
            .expect("rooms")
            .iter()
            .filter(|(room_id, _)| room_id.starts_with("json:notebook:"))
            .map(|(_, tx)| tx.clone())
            .collect();
        for tx in rooms {
            let (id, left) = (id.to_string(), left.clone());
            let cmd = Cmd::With(Box::new(move |room: &mut Room| {
                let names_it = room
                    .state_value("recovery")
                    .is_some_and(|r| r["id"] == id.as_str());
                if names_it {
                    room.set_recovery(left.as_ref())
                } else {
                    Default::default()
                }
            }));
            // A room that has closed has nothing to clear.
            let _ = tx.send(cmd);
        }
    }

    /// How many rooms are open.
    pub fn open_rooms(&self) -> usize {
        self.rooms.lock().expect("rooms").len()
    }

    /// A read of copy `id` found nothing, so the copy is gone from the
    /// store while a room may still name it: another server on this root
    /// discarded or pruned it. Every open room that names it takes the
    /// newest copy of its path left, or none.
    pub(crate) async fn recovery_gone(&self, id: &str) {
        for path in self.rooms_naming(id).await {
            self.forget_recovery(id, Some(&path)).await;
        }
    }

    /// The paths of the open rooms whose `state.recovery` names `id`.
    async fn rooms_naming(&self, id: &str) -> Vec<String> {
        let open: Vec<mpsc::UnboundedSender<Cmd>> = self
            .rooms
            .lock()
            .expect("rooms")
            .iter()
            .filter(|(room_id, tx)| room_id.starts_with("json:notebook:") && !tx.is_closed())
            .map(|(_, tx)| tx.clone())
            .collect();
        let wanted = id.to_string();
        let asked = open.into_iter().map(|tx| {
            let wanted = wanted.clone();
            async move {
                let (reply, rx) = tokio::sync::oneshot::channel();
                tx.send(Cmd::Info(reply)).ok()?;
                let info = tokio::time::timeout(Duration::from_secs(2), rx)
                    .await
                    .ok()?
                    .ok()?;
                (info.recovery.as_ref().and_then(|r| r.get("id")) == Some(&json!(wanted)))
                    .then_some(info.path)
            }
        });
        futures::future::join_all(asked)
            .await
            .into_iter()
            .flatten()
            .collect()
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

    /// The host API grant stored for the notebook at `path`, if any and
    /// its file is still the one it was granted to: one whose file was
    /// changed outside the server is dropped (#767).
    pub async fn grant(&self, path: &str) -> Option<String> {
        let _no_move = self.move_lock.read().await;
        let path = normalize_api_path(path);
        // Not while a copy or a new file is being made in the directory
        // and its path forgotten: a grant stored there is stale.
        let _lookup = self.lookups.register(&[&path]).await;
        let stamps = self.stamps.clone();
        self.with_ids(move |ids| {
            let id = ids.get(&path)?;
            checked_grant(ids, &stamps, &id, &path)
        })
        .await
    }

    /// The path the file with id `id` has now and its host API grant,
    /// checked as [`Collab::grant`] does, read together, so no rename
    /// lands between them; `None` once it's gone. The path is registered
    /// as a grant read does, once known, and read again under it; a path
    /// that changed meanwhile starts over.
    pub async fn path_and_grant(&self, id: &str) -> Option<(String, Option<String>)> {
        let _no_move = self.move_lock.read().await;
        loop {
            let path = self.path_of(id).await?;
            let _lookup = self.lookups.register(&[&path]).await;
            let (id, stamps) = (id.to_string(), self.stamps.clone());
            let found = self
                .with_ids(move |ids| {
                    let path = ids.path(&id)?;
                    let grant = checked_grant(ids, &stamps, &id, &path);
                    Some((path, grant))
                })
                .await?;
            // Under the read lock and the registration, the path can't
            // change in this process: only another process sharing the
            // file-id database could move it. Kept for that, and no
            // test holds it.
            if found.0 == path {
                return Some(found);
            }
        }
    }

    /// Before the server writes the file at `path`: drops its grant if the
    /// file changed outside the server. What comes back is for
    /// [`Collab::restamp`] after the write: `None`, no grant stands, so
    /// none is stamped; `Some(stamp)`, the stamp it is stored under.
    pub(crate) async fn check_for_write(&self, path: &str) -> Option<Option<String>> {
        let path = normalize_api_path(path);
        let stamps = self.stamps.clone();
        self.with_ids(move |ids| {
            let id = ids.get(&path)?;
            let mut missing = false;
            // A file that can't be seen for certain (a save of it is in
            // flight, or it is gone) says nothing about the grant, and
            // the check returns no stamp, so the write after it stamps
            // nothing.
            let seen = ids.check_for_write_with(&id, || match stamps.look(&path) {
                crate::stamping::Look::Stamp(s) => Some(s),
                crate::stamping::Look::Missing => {
                    missing = true;
                    None
                }
                crate::stamping::Look::Unstable => None,
            });
            // A file that is gone is about to be made anew: a grant that
            // never had a stamp would take it.
            if missing {
                ids.drop_unstamped(&id);
            }
            seen
        })
        .await
    }

    /// The server wrote the file at `path`: its grant keeps matching,
    /// if it is still stored as [`Collab::check_for_write`] found it
    /// (`seen`). Fails when the grant store does.
    pub(crate) async fn restamp(
        &self,
        path: &str,
        seen: &Option<Option<String>>,
    ) -> Result<(), String> {
        let seen = seen.clone();
        let path = normalize_api_path(path);
        let stamps = self.stamps.clone();
        self.with_ids(move |ids| {
            let Some(id) = ids.get(&path) else {
                return Ok(());
            };
            // The stamp is taken with the store held, so it is the latest.
            ids.restamp_if(&id, &seen, |_| stamps.own(&path))
                .map_err(|e| format!("restamping the host grant for {path}: {e}"))
        })
        .await
    }

    /// The grants other contents roots hold on the file at `path` (a
    /// `ganymede grant` run in a project directory, another server on a
    /// parent folder), found in the shared file id database and checked,
    /// as a write by this server is about to change the file: a grant
    /// belongs to the (root, path) it was made under, and this server's
    /// write is as much Ganymede's own under theirs. Nothing unless the
    /// ids are in a database. The in-flight marks are this process's, so
    /// another process reading one of these grants between the write and
    /// [`Collab::restamp_others`] drops it, which fails safe.
    pub(crate) async fn check_others(&self, path: &str) -> Vec<ganymede_contents::Held> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let file = root.join(normalize_api_path(path));
        self.with_ids(move |ids| {
            let mut found = ganymede_contents::held_with(ids, &file, Some(&root));
            found.iter_mut().for_each(|h| h.check());
            found
        })
        .await
    }

    /// After the write: the grants [`Collab::check_others`] found keep
    /// matching the file.
    pub(crate) async fn restamp_others(&self, held: Vec<ganymede_contents::Held>) {
        let failures = self
            .with_ids(move |_| {
                held.iter()
                    .filter_map(|h| h.restamp().err())
                    .collect::<Vec<_>>()
            })
            .await;
        for e in failures {
            tracing::warn!("{e}");
        }
    }

    /// A file was just made at `path`, by a copy or `new_untitled`, under
    /// [`Collab::begin_create`]'s guard: it has no grant, whatever is
    /// stored at the path under this root or another, left by a file
    /// deleted outside, so those go, and rooms open on the path close.
    /// The guard holds the move lock for reading, so no rename, delete or
    /// [`file_gone`] lands meanwhile, and claims the directory's
    /// children (a new folder: its directory's whole subtree), so no
    /// grant read, index or room open at a path directly in it looks at
    /// the path before this is done.
    pub(crate) async fn forget_new_file(&self, path: &str) {
        self.removed(path).await;
        self.removed_in_other_roots(path).await;
    }

    /// Stores `grant` (JSON) for the notebook with file id `id`, or
    /// removes it, stamped with its file as it is now, or not stamped if
    /// a save of the file is in flight (the first use stamps it then).
    pub async fn set_grant(&self, id: String, grant: Option<String>) -> Result<(), String> {
        let stamps = self.stamps.clone();
        self.with_ids(move |ids| ids.set_grant_with(&id, grant.as_deref(), |p| stamps.current(p)))
            .await
    }

    /// Whether the file ids, and so grants, outlive the server.
    pub async fn is_persistent(&self) -> bool {
        self.with_ids(|ids| ids.is_persistent()).await
    }

    /// The id for `path`, made the first time; and whether it was new.
    pub(crate) async fn index(&self, path: &str) -> (String, bool) {
        let path = path.to_string();
        self.with_ids(move |ids| ids.index(&path)).await
    }

    /// The id for the file at `path`, as [`Collab::index`], if `contents`
    /// has one matching `options` (a notebook, say), or the contents
    /// error. Under [`Collab::begin_move`]'s lock for reading, so no
    /// rename or delete through the contents API lands between the check
    /// and the id: a path that is gone never gets one. The path is
    /// registered as a grant read does, so a file being made at it
    /// doesn't get the id of a stale one.
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
        let _lookup = self.lookups.register(&[path]).await;
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
        self.delete_checked(contents, path, None).await.map(|_| ())
    }

    /// Whether the room on the file at `path` has a client, or may have:
    /// a room that doesn't answer within two seconds (busy with a slow
    /// save, which may hold unsaved edits) counts as in use. No room, or
    /// one that stopped, is not.
    async fn room_in_use(&self, path: &str) -> bool {
        let path = normalize_api_path(path);
        let Some(file_id) = self.with_ids(move |ids| ids.get(&path)).await else {
            return false;
        };
        let tx = self
            .rooms
            .lock()
            .expect("rooms")
            .get(&format!("json:notebook:{file_id}"))
            .filter(|tx| !tx.is_closed())
            .cloned();
        let Some(tx) = tx else {
            return false;
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        if tx.send(Cmd::Info(reply)).is_err() {
            return false;
        }
        match tokio::time::timeout(Duration::from_secs(2), rx).await {
            Ok(Ok(info)) => !info.clients.is_empty(),
            // No answer: busy, or a room with no file to describe.
            _ => true,
        }
    }

    /// [`Collab::delete`] of a file only if it is still as a caller saw
    /// it: its [`ganymede_contents::fileid::api_file_stamp`] is `stamp`
    /// and no room on it has a client (which may hold edits not saved
    /// yet, and a delete closes the room without saving). Otherwise the
    /// file stays; see [`DeleteIf`].
    ///
    /// The checks and the delete are under the move lock, so no rename,
    /// delete or new room lands between them. What the lock doesn't keep
    /// out is a write to the file that doesn't go through a room (another
    /// program, or a REST save), which is deleted with the file if it
    /// lands after the stamp is read.
    pub(crate) async fn delete_if_unchanged(
        self: &Arc<Self>,
        contents: &Arc<dyn Contents>,
        path: &str,
        stamp: &str,
    ) -> ganymede_contents::Result<DeleteIf> {
        self.delete_checked(contents, path, Some(stamp.to_string()))
            .await
    }

    async fn delete_checked(
        self: &Arc<Self>,
        contents: &Arc<dyn Contents>,
        path: &str,
        only_if: Option<String>,
    ) -> ganymede_contents::Result<DeleteIf> {
        let (collab, contents, path) = (self.clone(), contents.clone(), path.to_string());
        detached(async move {
            let _deleting = collab.begin_move().await;
            if let Some(stamp) = only_if {
                if collab.room_in_use(&path).await {
                    return Ok(DeleteIf::InUse);
                }
                let now = collab
                    .root()
                    .and_then(|r| ganymede_contents::fileid::api_file_stamp(r, &path));
                if now.as_deref() != Some(stamp.as_str()) {
                    return Ok(DeleteIf::Changed);
                }
            }
            contents.delete(&path).await?;
            collab.removed(&path).await;
            collab.removed_in_other_roots(&path).await;
            Ok(DeleteIf::Deleted)
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
            // A rename onto itself moves nothing: the ids and grants stay.
            if normalize_api_path(&old) == normalize_api_path(&new) {
                return contents.rename(&old, &new).await;
            }
            let _moving = collab.begin_move().await;
            // Writes under either path, so under a directory renamed
            // too, finish first and new ones wait; writes elsewhere go
            // on. Taken after the move lock, which nothing holds while
            // it waits for a write.
            let _no_saves = collab.stamps.begin_rename(&old, &new).await;
            // A file replaced outside loses its grant before the rename,
            // which would otherwise stamp the new file as the old one.
            let seen = collab.check_for_write(&old).await;
            // The same for the grants other roots hold on it. Both
            // restamps are only visible on a filesystem where a rename
            // changes the stamp, so no test can see them on Linux.
            let others = collab.check_others(&old).await;
            // A grant check mustn't look between the rename and the new
            // stamp, on a filesystem where a rename changes the stamp.
            // No test holds this mark: it is open only across the
            // rename on disk, which only a hook in the contents could
            // hold, and no contents has one.
            let _saving = collab.stamps.begin(&new);
            let model = contents.rename(&old, &new).await?;
            collab.moved(&old, &model.path).await;
            collab.moved_in_other_roots(&old, &model.path).await;
            if let Err(e) = collab.restamp(&model.path, &seen).await {
                tracing::warn!("{e}");
            }
            collab.restamp_others(others).await;
            Ok(model)
        })
        .await
    }

    /// The path a file id names now.
    pub(crate) async fn path_of(&self, id: &str) -> Option<String> {
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

    /// A file is about to be made in the directory `dir` by a copy or
    /// `new_untitled`, at a path not known until it is. Until the guard
    /// drops, after [`Collab::forget_new_file`]: no rename, delete or
    /// [`file_gone`] lands (the move lock, for reading, which a rename
    /// waits on for writing), and no grant read, index or room open at a
    /// path directly in `dir` (`""` for the root) looks at it, as it could
    /// still have the stale id or grant of a file deleted outside. A new
    /// file is one of these; a new folder's contents are deeper, so
    /// those creates claim the whole subtree. Looks elsewhere go on.
    ///
    /// `scope` is [`Scope::Children`] for a file, which is a direct child
    /// of `dir`, and [`Scope::Subtree`] when the new path may be a folder,
    /// whose ids and grants [`Collab::forget_new_file`] drops under the
    /// whole prefix, so a lookup inside it must wait too.
    pub(crate) async fn begin_create(&self, dir: &str, scope: Scope) -> Creating<'_> {
        let moves = self.move_lock.read().await;
        let claim = self.lookups.claim_scoped(&[dir], scope).await;
        Creating {
            _claim: claim,
            _moves: moves,
        }
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

    /// As [`Collab::moved`] for the ids other roots hold on the same
    /// files in the shared database (a project directory's), which follow
    /// with their grants. The stamp of a file doesn't change with its
    /// path, so they need no restamp.
    async fn moved_in_other_roots(&self, old: &str, new: &str) {
        let (Some(old), Some(new)) = (self.abs_path(old), self.abs_path(new)) else {
            return;
        };
        self.with_ids(move |ids| ids.moved_in_other_roots(&old, &new))
            .await;
    }

    /// `path` was deleted through this root: the ids, and so the grants,
    /// that other roots above it hold under it go too, as
    /// [`Collab::moved_in_other_roots`] moves them.
    async fn removed_in_other_roots(&self, path: &str) {
        let Some(path) = self.abs_path(path) else {
            return;
        };
        self.with_ids(move |ids| ids.removed_in_other_roots(&path))
            .await;
    }

    /// The absolute path of the API path `api`, with its directory
    /// resolved, as the file ids database names files.
    fn abs_path(&self, api: &str) -> Option<std::path::PathBuf> {
        let file = self.root.as_ref()?.join(normalize_api_path(api));
        Some(file.parent()?.canonicalize().ok()?.join(file.file_name()?))
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

/// What [`Collab::delete_if_unchanged`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeleteIf {
    Deleted,
    /// The file's stamp isn't the one given, or can't be read (the
    /// contents aren't a local directory).
    Changed,
    /// A room on the file has a client, or didn't say within two seconds.
    InUse,
}

pub(crate) enum Cmd {
    /// A client joins, with the name the server knows it by, if any, and
    /// how the room lists it.
    Join(ConnId, ClientTx, Option<String>, Listing),
    /// Who is in the room and what last changed; see [`Collab::list_rooms`].
    /// Not answered by a room with no file (awareness only).
    Info(tokio::sync::oneshot::Sender<RoomInfo>),
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

/// How a connection to a room is listed by [`Collab::list_rooms`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// A room client, named as its edits are credited.
    Person,
    /// An MCP connection's [`Hold`], named by its client.
    Agent(String),
    /// Not listed: a hold made for a kernel, with no one behind it.
    Unlisted,
}

/// What [`Collab::list_rooms`] asks a notebook room for.
pub(crate) struct RoomInfo {
    path: String,
    clients: Vec<ListedClient>,
    last_change: Option<(std::time::SystemTime, String, bool)>,
    /// The copy the room names in `state.recovery`, if any: read to
    /// find rooms naming a copy that is gone ([`Collab::recovery_gone`]).
    recovery: Option<Value>,
}

struct ListedClient {
    name: String,
    agent: bool,
}

/// A notebook room open now, as `GET /api/collaboration/rooms` lists it.
pub(crate) struct ListedRoom {
    pub(crate) path: String,
    pub(crate) file_id: String,
    pub(crate) clients: Vec<RoomClient>,
    /// When and by whom a cell's source or type, or the set of cells,
    /// last changed while the room was open.
    pub(crate) last_change: Option<RoomChange>,
}

/// Someone in a room: a person's name (from the room's awareness, or the
/// tailnet login), or an agent's (the MCP client's name).
pub(crate) struct RoomClient {
    pub(crate) name: String,
    pub(crate) agent: bool,
}

pub(crate) struct RoomChange {
    /// RFC 3339, UTC.
    pub(crate) at: String,
    pub(crate) by: String,
    pub(crate) person: bool,
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

/// `GET /api/collaboration/rooms`: the notebook rooms open now, with who
/// is in each and what last changed.
pub async fn rooms(State(state): State<AppState>) -> Json<Value> {
    let rooms: Vec<Value> = state
        .collab
        .list_rooms()
        .await
        .into_iter()
        .map(|r| {
            let clients: Vec<Value> = r
                .clients
                .into_iter()
                .map(|c| json!({"name": c.name, "kind": if c.agent { "agent" } else { "person" }}))
                .collect();
            let last_change = r
                .last_change
                .map(|c| json!({"at": c.at, "by": c.by, "person": c.person}));
            json!({
                "path": r.path,
                "file_id": r.file_id,
                "format": "json",
                "type": "notebook",
                "clients": clients,
                "last_change": last_change,
            })
        })
        .collect();
    Json(json!({ "rooms": rooms }))
}

#[derive(Deserialize)]
pub(crate) struct RecoveryQuery {
    path: Option<String>,
}

/// This server's recovery copies: the store its root sees. None of the
/// routes run without one, so a server that keeps no copies answers
/// `404` before it names any file.
fn recovery_store(state: &AppState) -> Result<recovery::Store, HttpError> {
    state
        .collab
        .recovery
        .clone()
        .ok_or_else(|| HttpError(404, "this server keeps no recovery copies".into()))
}

/// `id` as a recovery copy's id; it becomes a file name.
fn recovery_id(id: &str) -> Result<(), HttpError> {
    if recovery::valid_id(id) {
        Ok(())
    } else {
        Err(HttpError(400, format!("{id:?} is not a recovery copy id")))
    }
}

fn recovery_io(e: std::io::Error) -> HttpError {
    HttpError(500, format!("recovery copies: {e}"))
}

/// `GET /api/collaboration/recovery?path=`: the recovery copies kept for
/// the notebook at `path`, newest first, without their content.
pub(crate) async fn recovery_list(
    State(state): State<AppState>,
    Query(q): Query<RecoveryQuery>,
) -> Result<Json<Value>, HttpError> {
    let store = recovery_store(&state)?;
    let path = q
        .path
        .map(|p| normalize_api_path(&p))
        .ok_or_else(|| HttpError(400, "'path' parameter was not provided.".into()))?;
    let copies = blocking(move || recovery::list(&store, &path))
        .await
        .map_err(recovery_io)?;
    let copies: Vec<Value> = copies.iter().map(recovery::Meta::public).collect();
    Ok(Json(json!({ "copies": copies })))
}

/// `GET /api/collaboration/recovery/{id}`: a copy and its notebook, as
/// `{...meta, "content": <notebook JSON>}`. A client restores it by
/// saving `content` through the contents API or a room.
pub(crate) async fn recovery_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HttpError> {
    recovery_id(&id)?;
    let store = recovery_store(&state)?;
    let wanted = id.clone();
    let found = blocking(move || recovery::read(&store, &wanted))
        .await
        .map_err(recovery_io)?;
    let (meta, content) = match found {
        Some(found) => found,
        None => {
            // A room may still name the id: another server on this root
            // shares the store and may have discarded or pruned the
            // copy. It takes the newest copy left, or none.
            state.collab.recovery_gone(&id).await;
            return Err(HttpError(404, "no such recovery copy".into()));
        }
    };
    let mut body = meta.public();
    body["content"] = content;
    Ok(Json(body))
}

/// `DELETE /api/collaboration/recovery/{id}`: discard a copy. A room that
/// names it in `state.recovery` names the next newest of that path, if
/// any.
pub(crate) async fn recovery_discard(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    recovery_id(&id)?;
    let store = recovery_store(&state)?;
    let named = id.clone();
    let (existed, path) = blocking(move || {
        let path = match recovery::read_meta(&store, &named) {
            Ok(meta) => meta.map(|m| m.path),
            Err(e) => {
                // Discardable all the same; only its path isn't known.
                tracing::warn!(id = %named, "unreadable recovery copy: {e}");
                None
            }
        };
        Ok((recovery::discard(&store, &named)?, path))
    })
    .await
    .map_err(recovery_io)?;
    if !existed {
        return Err(HttpError(404, "no such recovery copy".into()));
    }
    state.collab.forget_recovery(&id, path.as_deref()).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn room(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<RoomQuery>,
    user: Option<axum::Extension<crate::auth::TailnetUser>>,
    headers: HeaderMap,
) -> Response {
    let user = user.map(|axum::Extension(u)| u.0);
    if !origin_allowed(&state, &headers) {
        return HttpError(403, "origin not allowed".into()).into_response();
    }
    // No message larger than a notebook the contents API would take.
    // Browsers send each message as one frame, so the frame limit (16 MiB
    // by default) has to match.
    ws.max_message_size(state.max_body_size)
        .max_frame_size(state.max_body_size)
        .on_upgrade(move |socket| serve_socket(socket, state, room_id, query.session_id, user))
}

async fn serve_socket(
    mut socket: WebSocket,
    state: AppState,
    room_id: String,
    session_id: Option<String>,
    user: Option<String>,
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
    let join = Cmd::Join(conn, out_tx, user, Listing::Person);
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

/// A file being made in a directory, until dropped: see
/// [`Collab::begin_create`].
pub(crate) struct Creating<'a> {
    _claim: crate::path_locks::Claimed,
    _moves: tokio::sync::RwLockReadGuard<'a, ()>,
}

/// Send `cmd` to `room_id`, opening the room if needed, and return its
/// sender: the one way into a room, for websocket clients and for the
/// server's own callers. An open room takes `cmd` with no path lookup;
/// an entry whose task has stopped is replaced by a fresh room. Opening a
/// notebook room (`file_id` given) looks its path up holding
/// [`Collab::begin_move`]'s lock for reading, so no rename is between its
/// disk change and the ids, and the path registered in `lookups`, so no
/// file is being made at it, until the room is open; a rename or delete
/// finished meanwhile changes `moves`, and the path is looked up again.
/// `None` when the file id is gone.
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
        // The path is only known once looked up: register it, which waits
        // while a create claims a directory above it, and look again. A
        // path that is the same under the registration can't be one a
        // create is about to make.
        let (path, _lookup, read) = loop {
            let path = collab.path_of(id).await?;
            let lookup = collab.lookups.register(&[&path]).await;
            let read = collab.moves.load(Ordering::SeqCst);
            if collab.path_of(id).await? == path {
                break (path, lookup, read);
            }
        };
        let mut rooms = collab.rooms.lock().expect("rooms");
        // The read lock keeps out renames and deletes through the contents
        // API; a change here could only come from a caller of `removed`
        // that doesn't hold it for writing. `forget_new_file`, which a
        // create runs under the read lock, is one, and it may have
        // dropped the id: the loop just starts over and looks it up again.
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

/// The grant of file id `id`, at `path`, if its file still matches its
/// stamp; one that doesn't is dropped. Looks at the file only when there
/// is a grant to check.
fn checked_grant(
    ids: &FileIds,
    stamps: &crate::stamping::Stamps,
    id: &str,
    path: &str,
) -> Option<String> {
    // The stamp is taken with the store held: see `grant_checked_with`.
    ids.grant_checked_with(id, || stamps.current(path))
}

/// Runs `f` on its own task and waits for it: dropping the wait leaves
/// `f` running to the end. A panic in `f` comes back as an I/O error.
pub(crate) async fn detached<T: Send + 'static>(
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
/// that never reads its frames. `listing` says how the room lists it in
/// its clients: an agent by its MCP client name, or not at all (a
/// kernel's). `None` when the room can't be opened.
pub(crate) async fn hold_notebook(
    collab: &Arc<Collab>,
    contents: &Arc<dyn Contents>,
    room_id: &str,
    listing: Listing,
) -> Option<Hold> {
    let conn = collab.next_conn.fetch_add(1, Ordering::Relaxed);
    // The receiver is dropped: frames sent to this client go nowhere.
    let (out, _, _) = ClientTx::new(usize::MAX);
    let room = send_to_notebook_room(
        collab,
        contents,
        room_id,
        Cmd::Join(conn, out, None, listing),
    )
    .await?;
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
    /// The server whose recovery dir the room keeps copies in, if it has
    /// one.
    recovery: Option<Arc<Collab>>,
    /// The last error of a failed save, until a save succeeds.
    save_error: Option<String>,
    /// Edits the disk won over the room's that have no recovery copy yet,
    /// oldest first. While there are any, clients are told that saving
    /// failed, however the saves since went: until each has its copy, or a
    /// client saves the room as it is now.
    lost: Vec<Lost>,
}

/// Unsaved edits that a merge with the disk lost, and that could not be
/// kept.
struct Lost {
    /// The room's notebook before the merge; `None` when it wasn't a
    /// notebook, so there is nothing a copy could hold.
    notebook: Option<Notebook>,
    /// The conflicting parts, and the cells among them.
    detail: String,
    cells: Vec<String>,
    /// What clients are told.
    error: String,
}

/// How many [`Lost`] edits a room holds on to; the oldest go first.
const MAX_LOST: usize = 20;

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
            recovery: None,
            save_error: None,
            lost: Vec::new(),
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
    // The MCP client names of the connections that are agents' holds.
    let mut agents: HashMap<ConnId, String> = HashMap::new();
    // The connections that are not listed at all.
    let mut unlisted: HashSet<ConnId> = HashSet::new();
    let (mut room, mut disk) = match &path {
        None => (Room::transient(), None),
        Some(path) => match load(contents.as_ref(), path).await {
            Ok((nb, mut disk)) => {
                let mut room = Room::notebook_room(&nb, path);
                room.saved(0, &disk.hash);
                disk.track(None);
                disk.recovery = collab.recovery.is_some().then(|| collab.clone());
                // A copy kept before, by this run or an earlier one, stays
                // on offer until it is restored or discarded.
                if let Some(store) = &collab.recovery {
                    let (store, p) = (store.clone(), path.clone());
                    match blocking(move || recovery::list(&store, &p)).await {
                        Ok(copies) => {
                            room.set_recovery(copies.first().map(recovery::Meta::summary).as_ref());
                        }
                        Err(e) => {
                            tracing::warn!(room = %room_id, "cannot list recovery copies of {path}: {e}")
                        }
                    }
                }
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
    // The generation of the room's content a recovery copy was kept for
    // before closing, so a close that finds a client arriving doesn't keep
    // another of the same.
    let mut recovered_at: Option<u64> = None;
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
                    Cmd::Join(conn, out, user, listing) => {
                        clients.insert(conn, out);
                        match listing {
                            Listing::Person => {}
                            Listing::Agent(name) => { agents.insert(conn, name); }
                            Listing::Unlisted => { unlisted.insert(conn); }
                        }
                        empty_since = None;
                        let fx = match &user {
                            Some(name) => room.connect_as(conn, name),
                            None => room.connect(conn),
                        };
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
                    Cmd::Info(reply) => {
                        if let Some(d) = disk.as_ref() {
                            let mut listed: Vec<ListedClient> = room
                                .client_names()
                                .into_iter()
                                .filter(|(conn, _)| !unlisted.contains(conn))
                                .map(|(conn, name)| match agents.get(&conn) {
                                    Some(agent) => ListedClient { name: agent.clone(), agent: true },
                                    None => ListedClient { name, agent: false },
                                })
                                .collect();
                            // People first; each group stays in join order.
                            listed.sort_by_key(|c| c.agent);
                            let last_change = room
                                .last_change()
                                .map(|(at, by)| (at, by.name.clone(), by.person));
                            let recovery = room.state_value("recovery");
                            let _ = reply.send(RoomInfo { path: d.path.clone(), clients: listed, last_change, recovery });
                        }
                    }
                    Cmd::Leave(conn) => {
                        clients.remove(&conn);
                        agents.remove(&conn);
                        unlisted.remove(&conn);
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
                        let Some(d) = disk.as_mut() else { continue };
                        if !is_under(&d.path, &gone) { continue }
                        // The file id is gone, so the room can't stay: if
                        // the copy can't be kept, the failure is logged.
                        let kept = kept_before_closing(&mut room, d, &clients, &mut recovered_at, recovery::Reason::FileGone).await;
                        if !kept || !keep_lost(&mut room, d, &clients).await {
                            tracing::error!(room = %room_id, path = %d.path, "closing the room without a recovery copy of its unsaved edits");
                        }
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
                        // The server is stopping: what the file didn't get
                        // is lost with the room unless it has a copy.
                        if let Some(d) = disk.as_mut() {
                            let reason = if room.is_read_only() {
                                recovery::Reason::ReadOnly
                            } else {
                                recovery::Reason::SaveFailed
                            };
                            let kept = kept_before_closing(&mut room, d, &clients, &mut recovered_at, reason).await;
                            if !kept || !keep_lost(&mut room, d, &clients).await {
                                tracing::error!(room = %room_id, path = %d.path, "stopping without a recovery copy of the room's unsaved edits");
                            }
                        }
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
                    let reloaded = reload(contents.as_ref(), &mut room, d, &clients).await;
                    // Saving now would write the deleted file back.
                    if reloaded.gone && file_gone(&collab, contents.as_ref(), &d.path).await { continue }
                    let needs_save = reloaded.needs_save;
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
                    let polled = poll(contents.as_ref(), &mut room, d, &clients).await;
                    if polled.gone && file_gone(&collab, contents.as_ref(), &d.path).await { continue }
                    if polled.needs_save {
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
                    // Why the room closes with edits the file doesn't have.
                    let mut closing = None;
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
                        closing = Some(if gone { recovery::Reason::FileGone } else { recovery::Reason::SaveFailed });
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
                        closing = Some(recovery::Reason::ReadOnly);
                    }
                    // No room retires holding edits without a copy of them
                    // on disk: if the copy can't be kept, the room stays
                    // and tries again.
                    if let Some(reason) = closing {
                        if !kept_before_closing(&mut room, d, &clients, &mut recovered_at, reason).await {
                            empty_since = Some(Instant::now());
                            continue;
                        }
                    }
                    if !keep_lost(&mut room, d, &clients).await {
                        empty_since = Some(Instant::now());
                        continue;
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
            Cmd::Join(_, out, _, _) => {
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
    disk.save_error = None;
    let status = try_save(contents, room, disk, clients, force).await;
    if !disk.lost.is_empty() {
        if force && status == SaveStatus::Success {
            // A person saved the room as it is, without those edits.
            disk.lost.clear();
        } else {
            keep_lost(room, disk, clients).await;
        }
    }
    // What clients are told: the room's edits are in the file, or a save
    // of them failed and they are not.
    match status {
        SaveStatus::Failed if room.is_dirty() => {
            let error = disk
                .save_error
                .get_or_insert_with(|| "the file kept changing".into())
                .clone();
            dispatch(clients, room.set_save_state("failed", Some(&error)).send);
        }
        // Saved, or failed only because the file won a cell of the client's.
        SaveStatus::Success | SaveStatus::Failed => say_saved(room, disk, clients),
        SaveStatus::Skipped if room.is_read_only() && room.is_dirty() => {
            let error = "another server owns the file";
            dispatch(clients, room.set_save_state("failed", Some(error)).send);
        }
        SaveStatus::Skipped => {}
    }
    status
}

/// Tell clients that the file has the room's edits, unless edits the disk
/// won have no copy ([`Disk::lost`]).
fn say_saved(room: &mut Room, disk: &Disk, clients: &HashMap<ConnId, ClientTx>) {
    let fx = match disk.lost.last() {
        Some(lost) => room.set_save_state("failed", Some(&lost.error)),
        None => room.set_save_state("saved", None),
    };
    dispatch(clients, fx.send);
}

/// [`save_room`] without telling clients how it went.
async fn try_save(
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
                disk.save_error = Some(format!("the room's content is not a notebook: {e}"));
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
                let reloaded = reload(contents, room, disk, clients).await;
                if reloaded.gone {
                    // Deleted outside the API: the room's next look at the
                    // file closes it (#719).
                    disk.save_error = Some("the file was deleted".into());
                    return SaveStatus::Failed;
                }
                lost |= reloaded.lost;
            }
            Err(e) => {
                tracing::warn!(path = %disk.path, "room save failed: {e}");
                disk.save_error = Some(e.to_string());
                return SaveStatus::Failed;
            }
        }
    }
    SaveStatus::Failed
}

/// Merge the file in when it changed and it was not our write.
async fn poll(
    contents: &dyn Contents,
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
) -> Reloaded {
    let model = match contents.get(&disk.path, GetOptions::default()).await {
        Ok(model) => model,
        Err(ganymede_contents::Error::NotFound(_) | ganymede_contents::Error::Hidden(_)) => {
            return Reloaded {
                gone: true,
                ..Reloaded::default()
            };
        }
        Err(_) => return Reloaded::default(), // unreadable: keep the room as it is
    };
    if model.last_modified == disk.last_modified {
        return Reloaded::default();
    }
    reload(contents, room, disk, clients).await
}

/// The room's file was found gone from disk, deleted by another program
/// rather than through the contents API (#719). Under
/// [`Collab::begin_move`]'s lock, as [`Collab::delete`], and only if it is
/// still gone, drop its ids and close the rooms on it, so a file made
/// later at the path gets a new id and room instead of joining this one.
/// True when it did; the room's own [`Cmd::Removed`] is then queued.
async fn file_gone(collab: &Collab, contents: &dyn Contents, path: &str) -> bool {
    let _deleting = collab.begin_move().await;
    let gone = matches!(
        contents.get(path, GetOptions::default()).await,
        Err(ganymede_contents::Error::NotFound(_) | ganymede_contents::Error::Hidden(_))
    );
    if gone {
        tracing::info!(
            path,
            "file deleted outside the contents API; closing its room"
        );
        collab.removed(path).await;
    }
    gone
}

/// Run `f`, which does file I/O, off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

/// Keep `notebook`, the room's, as a recovery copy in `disk`'s recovery
/// dir. `Ok(None)` when there is no dir.
async fn keep_copy(
    disk: &Disk,
    notebook: Notebook,
    reason: recovery::Reason,
    detail: String,
    cells: Vec<String>,
) -> std::io::Result<Option<recovery::Meta>> {
    let Some(collab) = disk.recovery.clone() else {
        return Ok(None);
    };
    let Some(store) = collab.recovery.clone() else {
        return Ok(None);
    };
    let path = disk.path.clone();
    let (meta, pruned) =
        blocking(move || recovery::write(&store, &path, &notebook, reason, &detail, &cells))
            .await?;
    for gone in pruned {
        collab.forget_recovery(&gone.id, Some(&gone.path)).await;
    }
    Ok(Some(meta))
}

/// Before the room closes with edits the file doesn't have: keep a copy
/// of them and tell the room's clients, which are the ones about to find
/// it gone. False when the copy could not be made, and the room should
/// stay. True without a recovery dir, as before, for a room whose edits
/// already have a copy, and for one whose content is not a notebook: no
/// copy of it is possible.
async fn kept_before_closing(
    room: &mut Room,
    disk: &Disk,
    clients: &HashMap<ConnId, ClientTx>,
    recovered_at: &mut Option<u64>,
    reason: recovery::Reason,
) -> bool {
    if !room.is_dirty() || *recovered_at == Some(room.generation()) {
        return true;
    }
    let notebook = match room.notebook() {
        Some(Ok(nb)) => nb,
        Some(Err(e)) => {
            tracing::error!(path = %disk.path, "cannot keep a recovery copy: the room's content is not a notebook: {e}");
            return true;
        }
        None => return true,
    };
    let detail = match reason {
        recovery::Reason::SaveFailed => disk.save_error.clone().unwrap_or_default(),
        recovery::Reason::FileGone => "the file was deleted".to_string(),
        recovery::Reason::ReadOnly => "another server owns the file".to_string(),
        recovery::Reason::MergeConflict => String::new(),
    };
    match keep_copy(disk, notebook, reason, detail, Vec::new()).await {
        Ok(None) => true,
        Ok(Some(meta)) => {
            tracing::warn!(path = %disk.path, id = %meta.id, "kept a recovery copy of the room's unsaved edits");
            *recovered_at = Some(room.generation());
            dispatch(clients, room.set_recovery(Some(&meta.summary())).send);
            true
        }
        Err(e) => {
            tracing::error!(path = %disk.path, "cannot keep a recovery copy of the room's unsaved edits: {e}");
            false
        }
    }
}

/// The disk won `conflicts` over the room's edits: keep `ours`, the room's
/// notebook from before the merge, as a recovery copy and tell clients.
/// False when it can't be kept: the edits are gone from the room, so the
/// log has an error, and [`Disk::lost`] tells clients that saving failed
/// and tries again.
async fn keep_conflicting(
    room: &mut Room,
    disk: &mut Disk,
    clients: &HashMap<ConnId, ClientTx>,
    ours: Option<Result<Notebook, String>>,
    conflicts: &[String],
) -> bool {
    let detail = conflicts.join(", ");
    let mut lost = Lost {
        notebook: None,
        // "cell <id>" names a cell; the rest ("order", "metadata", "all") a
        // part of the notebook.
        cells: conflicts
            .iter()
            .filter_map(|c| c.strip_prefix("cell "))
            .map(str::to_owned)
            .collect(),
        error: format!("the disk won {detail}, and the room's edits have no recovery copy yet"),
        detail,
    };
    match ours {
        None => return true,
        Some(Ok(nb)) if disk.recovery.is_some() => lost.notebook = Some(nb),
        // With no recovery dir, only logged, as by the caller.
        Some(Ok(_)) => return true,
        Some(Err(e)) => {
            tracing::error!(path = %disk.path, "the disk won over the room's edits, which are not a notebook, so no copy of them can be kept: {e}");
            lost.error = format!(
                "the file replaced the room's edits, which could not be kept: they are not a notebook: {e}"
            );
        }
    }
    if disk.lost.len() >= MAX_LOST {
        let oldest = disk.lost.remove(0);
        tracing::error!(path = %disk.path, "too many edits without a recovery copy; dropping those to {}", oldest.detail);
    }
    disk.lost.push(lost);
    keep_lost(room, disk, clients).await;
    let kept = disk.lost.is_empty();
    if !kept {
        say_saved(room, disk, clients);
    }
    kept
}

/// Try to make the copies that [`Disk::lost`] edits still lack. False while
/// one that could have a copy doesn't; a room in that state stays open.
async fn keep_lost(room: &mut Room, disk: &mut Disk, clients: &HashMap<ConnId, ClientTx>) -> bool {
    let mut left = Vec::new();
    let mut kept_one = false;
    for mut lost in std::mem::take(&mut disk.lost) {
        let Some(notebook) = lost.notebook.clone() else {
            left.push(lost);
            continue;
        };
        let (reason, cells) = (recovery::Reason::MergeConflict, lost.cells.clone());
        match keep_copy(disk, notebook, reason, lost.detail.clone(), cells).await {
            Ok(Some(meta)) => {
                tracing::warn!(path = %disk.path, id = %meta.id, "kept the room's version of what the disk won");
                dispatch(clients, room.set_recovery(Some(&meta.summary())).send);
                kept_one = true;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(path = %disk.path, "cannot keep a recovery copy of the room's edits the disk won: {e}");
                lost.error = format!(
                    "the disk won {} and its copy of the room's edits could not be kept: {e}",
                    lost.detail
                );
                left.push(lost);
            }
        }
    }
    disk.lost = left;
    // Nothing else to say: the file has the rest of the room.
    if kept_one && !room.is_dirty() {
        say_saved(room, disk, clients);
    }
    disk.lost.iter().all(|l| l.notebook.is_none())
}

/// What [`reload`] did.
#[derive(Default)]
struct Reloaded {
    /// The room holds a merge that needs saving.
    needs_save: bool,
    /// The file won a cell the room had changed.
    lost: bool,
    /// The file is gone: deleted outside the contents API (#719).
    gone: bool,
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
            // The room's edits as they are before the disk takes the cells
            // they conflict over.
            let ours = match room.is_dirty() {
                true => room.notebook().map(|nb| nb.map_err(|e| e.to_string())),
                false => None,
            };
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
            let kept = conflicts.is_empty()
                || keep_conflicting(room, disk, clients, ours, &conflicts).await;
            if kept && !room.is_dirty() {
                // The file's version is the room's now.
                say_saved(room, disk, clients);
            }
            Reloaded {
                needs_save: room.is_dirty(),
                lost: !conflicts.is_empty(),
                gone: false,
            }
        }
        Err(e) if e.missing => {
            tracing::info!(path = %disk.path, "file deleted on disk");
            Reloaded {
                gone: true,
                ..Reloaded::default()
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

    /// A room's save goes through the server's contents, so the write
    /// observer hears of it once (#916). A room runs a notebook's outputs
    /// through the same save.
    #[tokio::test]
    async fn a_room_save_is_reported_to_the_write_observer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let nb = r#"{"cells":[{"cell_type":"markdown","id":"a","metadata":{},"source":["hello"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(root.join("a.ipynb"), nb).unwrap();
        let observer = Arc::new(crate::write_observer::WriteObserver::new());
        type Calls = Vec<(PathBuf, Option<String>, Option<String>)>;
        let calls: Arc<Mutex<Calls>> = Arc::default();
        let into = calls.clone();
        observer.set(Some(Arc::new(move |p, b, a| {
            into.lock().unwrap().push((
                p.to_path_buf(),
                b.map(str::to_string),
                a.map(str::to_string),
            ));
        })));
        let collab = Arc::new(
            Collab::new(FileIds::in_memory(), Some(root.clone())).with_write_observer(observer),
        );
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (loaded, mut disk) = load(contents.as_ref(), "a.ipynb").await.ok().unwrap();
        let mut room = Room::notebook_room(&loaded, "a.ipynb");
        room.saved(0, &disk.hash);
        room.edit(|txn, roots| {
            use ganymede_documents::yrs::{Array, Map, Out, Text};
            let Some(Out::YMap(cell)) = roots.cells.get(txn, 0) else {
                panic!("no first cell")
            };
            let Some(Out::YText(t)) = cell.get(txn, "source") else {
                panic!("no source")
            };
            t.insert(txn, 0, "x");
        });
        let before = ganymede_contents::file_stamp(&root.join("a.ipynb"));
        let status = save_room(
            contents.as_ref(),
            &mut room,
            &mut disk,
            &HashMap::new(),
            false,
        )
        .await;
        assert_eq!(status, SaveStatus::Success);
        let after = ganymede_contents::file_stamp(&root.join("a.ipynb"));
        assert!(before.is_some() && after.is_some() && before != after);
        assert_eq!(
            *calls.lock().unwrap(),
            [(root.join("a.ipynb"), before, after)]
        );
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

    /// Join `room_id` as a person the server knows as `name`.
    async fn join_as(
        collab: &Arc<Collab>,
        contents: &Arc<dyn Contents>,
        room_id: &str,
        conn: ConnId,
        name: &str,
    ) -> (mpsc::UnboundedSender<Cmd>, mpsc::Receiver<Out>) {
        let (out, rx, _) = ClientTx::new(usize::MAX);
        let tx = send_to_notebook_room(
            collab,
            contents,
            room_id,
            Cmd::Join(conn, out, Some(name.to_string()), Listing::Person),
        )
        .await
        .unwrap();
        (tx, rx)
    }

    /// Poll until `f` holds of the listing, or fail after five seconds.
    async fn listed_until(
        collab: &Collab,
        what: &str,
        f: impl Fn(&[ListedRoom]) -> bool,
    ) -> Vec<ListedRoom> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let rooms = collab.list_rooms().await;
            if f(&rooms) {
                return rooms;
            }
            assert!(Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// No rooms, no list; awareness-only rooms are left out (#893).
    #[tokio::test]
    async fn listing_rooms_leaves_out_none_and_transient_ones() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        assert!(collab.list_rooms().await.is_empty());

        let (cmd, rx) = report_loaded();
        send_to_room(&collab, &contents, "Test:live", None, cmd)
            .await
            .unwrap();
        loads(rx, "the transient room opened").await;
        assert_eq!(collab.open_rooms(), 1);
        assert!(collab.list_rooms().await.is_empty());
    }

    /// A room with a person and an agent lists both, the person first, with
    /// the path and file id; a second person is a second client; a hold
    /// releasing, and the clients leaving, empty it; and it disappears once
    /// its cleanup delay passes (#893).
    #[tokio::test]
    async fn listing_rooms_follows_clients_and_closing() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(
            Collab::new(FileIds::in_memory(), None).with_cleanup_delay(Duration::from_millis(50)),
        );
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let hold = hold_notebook(
            &collab,
            &contents,
            &room_id,
            Listing::Agent("claude-code".into()),
        )
        .await
        .unwrap();
        let (one, _rx1) = join_as(&collab, &contents, &room_id, 9001, "Ana").await;
        let (two, _rx2) = join_as(&collab, &contents, &room_id, 9002, "Ben").await;

        let rooms = collab.list_rooms().await;
        assert_eq!(rooms.len(), 1);
        let room = &rooms[0];
        assert_eq!(room.path, "a.ipynb");
        assert_eq!(format!("json:notebook:{}", room.file_id), room_id);
        let who: Vec<(&str, bool)> = room
            .clients
            .iter()
            .map(|c| (c.name.as_str(), c.agent))
            .collect();
        assert_eq!(who, [("Ana", false), ("Ben", false), ("claude-code", true)]);
        assert!(room.last_change.is_none());

        // The agent's hold releases: it is gone from the clients.
        drop(hold);
        listed_until(&collab, "the hold to leave", |r| r[0].clients.len() == 2).await;
        // One person leaves.
        one.send(Cmd::Leave(9001)).unwrap();
        let rooms = listed_until(&collab, "Ana to leave", |r| r[0].clients.len() == 1).await;
        assert_eq!(rooms[0].clients[0].name, "Ben");

        // The last leaves: the room stays listed, empty, until the cleanup
        // delay passes, then it is gone.
        two.send(Cmd::Leave(9002)).unwrap();
        listed_until(&collab, "the room to retire", |r| r.is_empty()).await;
        assert_eq!(collab.open_rooms(), 0);
    }

    /// A hold made for a kernel has no one behind it: the room is listed
    /// with no clients (#893).
    #[tokio::test]
    async fn listing_rooms_leaves_out_unlisted_holds() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let _kernel = hold_notebook(&collab, &contents, &room_id, Listing::Unlisted)
            .await
            .unwrap();
        let rooms = collab.list_rooms().await;
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].clients.is_empty());
        // Beside a person and an agent, it is still not there.
        let _agent = hold_notebook(&collab, &contents, &room_id, Listing::Agent("a".into()))
            .await
            .unwrap();
        let (_p, _rx) = join_as(&collab, &contents, &room_id, 9201, "Ana").await;
        let names: Vec<String> = collab.list_rooms().await[0]
            .clients
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, ["Ana", "a"]);
    }

    /// A room that doesn't answer is left out after two seconds, and the
    /// others are listed with it, not after it (#893).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listing_rooms_leaves_out_a_room_that_does_not_answer() {
        let (dir, contents) = notebook_dir();
        let nb = r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        std::fs::write(dir.path().join("b.ipynb"), nb).unwrap();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let a = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let b = notebook_room_id(&collab, &contents, "b.ipynb")
            .await
            .unwrap();
        let _ha = hold_notebook(&collab, &contents, &a, Listing::Agent("x".into()))
            .await
            .unwrap();
        let _hb = hold_notebook(&collab, &contents, &b, Listing::Agent("x".into()))
            .await
            .unwrap();
        assert_eq!(collab.list_rooms().await.len(), 2);

        // Room b is busy in a command that outlasts the wait.
        send_to_notebook_room(
            &collab,
            &contents,
            &b,
            Cmd::With(Box::new(|_: &mut Room| {
                std::thread::sleep(Duration::from_secs(4));
                Default::default()
            })),
        )
        .await
        .unwrap();
        let started = Instant::now();
        let rooms = collab.list_rooms().await;
        let took = started.elapsed();
        assert_eq!(rooms.len(), 1, "only the room that answered");
        assert_eq!(rooms[0].path, "a.ipynb");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    /// A rename moves the listed path, and the file id stays (#893).
    #[tokio::test]
    async fn listing_rooms_follows_a_rename() {
        let (dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let _hold = hold_notebook(&collab, &contents, &room_id, Listing::Agent("agent".into()))
            .await
            .unwrap();
        let before = collab.list_rooms().await;
        assert_eq!(before[0].path, "a.ipynb");

        std::fs::rename(dir.path().join("a.ipynb"), dir.path().join("b.ipynb")).unwrap();
        collab.moved("a.ipynb", "b.ipynb").await;
        let after = listed_until(&collab, "the path to move", |r| {
            r.first().is_some_and(|r| r.path == "b.ipynb")
        })
        .await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].file_id, before[0].file_id);
    }

    /// A room's last change is the edit made through it, by whoever the
    /// edit was credited to (#893).
    #[tokio::test]
    async fn listing_rooms_reports_the_last_change() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let _hold = hold_notebook(
            &collab,
            &contents,
            &room_id,
            Listing::Agent("claude-code".into()),
        )
        .await
        .unwrap();
        assert!(collab.list_rooms().await[0].last_change.is_none());

        with_notebook(&collab, &contents, &room_id, |room| {
            let ((), fx) = room.edit(|txn, roots| {
                ganymede_documents::schema::insert_cell(
                    txn,
                    &roots.cells,
                    0,
                    json!({"cell_type": "markdown", "id": "n", "metadata": {}, "source": "hi"}),
                );
            });
            room.set_author(
                &["n".to_string()],
                &ganymede_documents::Author {
                    name: "claude-code".into(),
                    person: false,
                },
            );
            ((), fx)
        })
        .await
        .unwrap();

        let change = collab.list_rooms().await[0]
            .last_change
            .take()
            .expect("a change");
        assert_eq!(change.by, "claude-code");
        assert!(!change.person);
        let at: jiff::Timestamp = change.at.parse().unwrap();
        let age = jiff::Timestamp::now().duration_since(at);
        assert!(age.as_secs() < 30, "{} is {age:?} old", change.at);
    }

    /// A name the server vouches for is cleaned as the attribution code
    /// cleans it: control characters dropped, at most 64 characters
    /// (#893).
    #[tokio::test]
    async fn listing_rooms_cleans_client_names() {
        let (_dir, contents) = notebook_dir();
        let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
        let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let long = "x".repeat(500);
        let (_a, _rxa) = join_as(
            &collab,
            &contents,
            &room_id,
            9101,
            "Mal\u{1b}[31mlory\n\u{7}",
        )
        .await;
        let (_b, _rxb) = join_as(&collab, &contents, &room_id, 9102, &long).await;
        let rooms = collab.list_rooms().await;
        let names: Vec<&str> = rooms[0].clients.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"Mal[31mlory"), "{names:?}");
        assert!(names.iter().all(|n| n.chars().count() <= 64), "{names:?}");
        assert!(
            names.iter().all(|n| !n.chars().any(char::is_control)),
            "{names:?}"
        );
    }

    /// A file made at a path whose id a room is open on closes the room,
    /// as a delete does: for a new untitled file and a copy.
    #[tokio::test]
    async fn a_new_file_closes_the_rooms_open_on_its_path() {
        for copy in [false, true] {
            let (dir, contents) = notebook_dir();
            let collab = Arc::new(Collab::new(FileIds::in_memory(), None));
            let stamped = collab.stamping(contents.clone());
            let stale = if copy {
                "a-Copy1.ipynb"
            } else {
                "Untitled.ipynb"
            };
            std::fs::copy(dir.path().join("a.ipynb"), dir.path().join(stale)).unwrap();
            let (id, _) = collab.index(stale).await;
            let (cmd, rx) = report_loaded();
            let room_id = format!("json:notebook:{id}");
            let tx = send_to_room(&collab, &contents, &room_id, Some(&id), cmd)
                .await
                .unwrap();
            loads(rx, "the room loaded").await;
            // Deleted outside, and made anew through the server.
            std::fs::remove_file(dir.path().join(stale)).unwrap();
            if copy {
                stamped.copy("a.ipynb", None).await.unwrap();
            } else {
                stamped
                    .new_untitled("", ContentType::Notebook, Some(".ipynb"))
                    .await
                    .unwrap();
            }
            assert!(dir.path().join(stale).is_file(), "made at {stale}");
            let mut closed = false;
            for _ in 0..40 {
                let (cmd, rx) = report_loaded();
                if tx.send(cmd).is_err()
                    || tokio::time::timeout(Duration::from_millis(50), rx)
                        .await
                        .is_ok_and(|r| r.is_err())
                {
                    closed = true;
                    break;
                }
            }
            assert!(closed, "the room on {stale} stayed open (copy: {copy})");
        }
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
        let hold = hold_notebook(&collab, &contents, &room_id, Listing::Unlisted)
            .await
            .unwrap();
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

#[cfg(test)]
mod delete_if_unchanged_tests {
    use super::*;
    use ganymede_contents::fileid::api_file_stamp;

    fn nb() -> &'static str {
        r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#
    }

    fn setup(root: bool) -> (tempfile::TempDir, Arc<Collab>, Arc<dyn Contents>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), nb()).unwrap();
        let root = root.then(|| dir.path().canonicalize().unwrap());
        let collab = Arc::new(Collab::new(FileIds::in_memory(), root));
        let contents: Arc<dyn Contents> =
            Arc::new(ganymede_contents::FsContents::new(dir.path(), Default::default()).unwrap());
        (dir, collab, contents)
    }

    #[tokio::test]
    async fn a_matching_stamp_deletes_the_file_and_its_id() {
        let (dir, collab, contents) = setup(true);
        let stamp = api_file_stamp(collab.root().unwrap(), "a.ipynb").unwrap();
        let (id, _) = collab
            .index_existing(&contents, "a.ipynb", GetOptions::default())
            .await
            .unwrap();
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::Deleted);
        assert!(!dir.path().join("a.ipynb").exists());
        assert_eq!(collab.path_of(&id).await, None, "the id went with it");
    }

    #[tokio::test]
    async fn a_stamp_that_changed_keeps_the_file() {
        let (dir, collab, contents) = setup(true);
        let stamp = api_file_stamp(collab.root().unwrap(), "a.ipynb").unwrap();
        std::fs::write(dir.path().join("a.ipynb"), format!("{} ", nb())).unwrap();
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::Changed);
        assert!(dir.path().join("a.ipynb").exists());
    }

    #[tokio::test]
    async fn contents_without_a_local_root_keep_the_file() {
        let (dir, collab, contents) = setup(false);
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", "1:2:3")
            .await;
        assert_eq!(r.unwrap(), DeleteIf::Changed);
        assert!(dir.path().join("a.ipynb").exists());
    }

    /// A room that doesn't answer within two seconds may be busy saving
    /// edits: the file stays.
    #[tokio::test(start_paused = true)]
    async fn a_room_that_does_not_answer_keeps_the_file() {
        let (dir, collab, contents) = setup(true);
        let stamp = api_file_stamp(collab.root().unwrap(), "a.ipynb").unwrap();
        let (id, _) = collab
            .index_existing(&contents, "a.ipynb", GetOptions::default())
            .await
            .unwrap();
        // A room whose receiver is kept and never read.
        let (tx, _rx) = mpsc::unbounded_channel();
        collab
            .rooms
            .lock()
            .unwrap()
            .insert(format!("json:notebook:{id}"), tx);
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::InUse);
        assert!(dir.path().join("a.ipynb").exists());
    }

    /// A client in another notebook's room doesn't keep this one.
    #[tokio::test]
    async fn a_client_in_another_notebooks_room_does_not_keep_the_file() {
        let (dir, collab, contents) = setup(true);
        std::fs::write(dir.path().join("b.ipynb"), nb()).unwrap();
        let stamp = api_file_stamp(collab.root().unwrap(), "a.ipynb").unwrap();
        let id = notebook_room_id(&collab, &contents, "b.ipynb")
            .await
            .unwrap();
        let _hold = hold_notebook(&collab, &contents, &id, Listing::Agent("x".into()))
            .await
            .unwrap();
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::Deleted);
        assert!(!dir.path().join("a.ipynb").exists());
        assert!(dir.path().join("b.ipynb").exists());
    }

    /// A room with a client may hold edits not saved yet, which closing
    /// it would drop: the file stays until the client leaves.
    #[tokio::test]
    async fn a_file_with_a_client_in_its_room_is_kept() {
        let (dir, collab, contents) = setup(true);
        let stamp = api_file_stamp(collab.root().unwrap(), "a.ipynb").unwrap();
        let id = notebook_room_id(&collab, &contents, "a.ipynb")
            .await
            .unwrap();
        let hold = hold_notebook(&collab, &contents, &id, Listing::Agent("x".into()))
            .await
            .unwrap();
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::InUse);
        assert!(dir.path().join("a.ipynb").exists());
        drop(hold);
        let r = collab
            .delete_if_unchanged(&contents, "a.ipynb", &stamp)
            .await;
        assert_eq!(r.unwrap(), DeleteIf::Deleted);
        assert!(!dir.path().join("a.ipynb").exists());
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::recovery::{self, Reason};
    use ganymede_contents::{FsContents, SaveRequest};
    use std::sync::atomic::AtomicBool;

    /// The file's notebook: cell `a` holds "keep".
    const NB: &str = r#"{"cells":[{"cell_type":"markdown","id":"a","metadata":{},"source":"keep"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;

    /// Contents on disk whose saves fail while `fail` is set.
    struct FailSaves {
        inner: FsContents,
        fail: Arc<AtomicBool>,
        /// Once a save has failed, the file is gone.
        vanish: Arc<AtomicBool>,
        vanished: AtomicBool,
    }

    #[async_trait::async_trait]
    impl Contents for FailSaves {
        async fn get(
            &self,
            path: &str,
            options: GetOptions,
        ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
            if self.vanished.load(Ordering::SeqCst) {
                return Err(ganymede_contents::Error::NotFound(path.into()));
            }
            self.inner.get(path, options).await
        }
        async fn save(
            &self,
            path: &str,
            request: SaveRequest,
        ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
            if self.fail.load(Ordering::SeqCst) {
                self.vanished
                    .store(self.vanish.load(Ordering::SeqCst), Ordering::SeqCst);
                return Err(std::io::Error::other("disk full").into());
            }
            self.inner.save(path, request).await
        }
        async fn delete(&self, path: &str) -> ganymede_contents::Result<()> {
            self.inner.delete(path).await
        }
        async fn rename(
            &self,
            old: &str,
            new: &str,
        ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
            self.inner.rename(old, new).await
        }
        async fn new_untitled(
            &self,
            dir: &str,
            kind: ContentType,
            ext: Option<&str>,
        ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
            self.inner.new_untitled(dir, kind, ext).await
        }
        async fn copy(
            &self,
            from: &str,
            to: Option<&str>,
        ) -> ganymede_contents::Result<ganymede_contents::ContentsModel> {
            self.inner.copy(from, to).await
        }
        async fn list_checkpoints(
            &self,
            path: &str,
        ) -> ganymede_contents::Result<Vec<ganymede_contents::Checkpoint>> {
            self.inner.list_checkpoints(path).await
        }
        async fn create_checkpoint(
            &self,
            path: &str,
        ) -> ganymede_contents::Result<ganymede_contents::Checkpoint> {
            self.inner.create_checkpoint(path).await
        }
        async fn restore_checkpoint(&self, path: &str, id: &str) -> ganymede_contents::Result<()> {
            self.inner.restore_checkpoint(path, id).await
        }
        async fn delete_checkpoint(&self, path: &str, id: &str) -> ganymede_contents::Result<()> {
            self.inner.delete_checkpoint(path, id).await
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        /// Holds the recovery dir, which is `data.path().join("recovery")`.
        data: tempfile::TempDir,
        collab: Arc<Collab>,
        contents: Arc<dyn Contents>,
        fail: Arc<AtomicBool>,
        vanish: Arc<AtomicBool>,
        room_id: String,
    }

    impl Fixture {
        fn recovery_dir(&self) -> PathBuf {
            self.data.path().join("recovery")
        }

        fn store(&self) -> recovery::Store {
            recovery::Store::new(&self.recovery_dir(), None)
        }

        fn copies(&self) -> Vec<recovery::Meta> {
            // None while a file is in the dir's place.
            recovery::list(&self.store(), "a.ipynb").unwrap_or_default()
        }

        /// A room with a recovery dir. With `blocked`, a file takes the
        /// dir's place once the server has it, so no copy can be written.
        async fn new(blocked: bool) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.ipynb"), NB).unwrap();
            let data = tempfile::tempdir().unwrap();
            let fail = Arc::new(AtomicBool::new(false));
            let vanish = Arc::new(AtomicBool::new(false));
            let contents: Arc<dyn Contents> = Arc::new(FailSaves {
                inner: FsContents::new(dir.path(), Default::default()).unwrap(),
                fail: fail.clone(),
                vanish: vanish.clone(),
                vanished: AtomicBool::new(false),
            });
            let collab = Arc::new(
                Collab::new(FileIds::in_memory(), None)
                    .with_recovery_dir(data.path().join("recovery"))
                    .with_cleanup_delay(Duration::from_secs(1)),
            );
            if blocked {
                let rec = data.path().join("recovery");
                std::fs::remove_dir(&rec).unwrap();
                std::fs::write(&rec, "not a directory").unwrap();
            }
            let room_id = notebook_room_id(&collab, &contents, "a.ipynb")
                .await
                .unwrap();
            Fixture {
                dir,
                data,
                collab,
                contents,
                fail,
                vanish,
                room_id,
            }
        }

        /// Join the room as a client.
        async fn join(&self, conn: ConnId) -> (mpsc::UnboundedSender<Cmd>, mpsc::Receiver<Out>) {
            let (out, rx, _) = ClientTx::new(usize::MAX);
            let tx = send_to_notebook_room(
                &self.collab,
                &self.contents,
                &self.room_id,
                Cmd::Join(conn, out, Some("me".into()), Listing::Person),
            )
            .await
            .unwrap();
            (tx, rx)
        }

        /// Make cell `a` say `text` in the room.
        async fn type_into_a(&self, text: &'static str) {
            with_notebook(&self.collab, &self.contents, &self.room_id, move |room| {
                let ((), fx) = room.edit(|txn, roots| {
                    assert!(crate::room_ops::set_source(txn, roots, "a", text, "me"));
                });
                ((), fx)
            })
            .await
            .unwrap();
        }

        async fn until_state(&self, key: &'static str, want: Value) {
            for _ in 0..30 {
                if self.state(key).await.as_ref() == Some(&want) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            panic!("state.{key} never became {want}");
        }

        async fn state(&self, key: &'static str) -> Option<Value> {
            with_notebook(&self.collab, &self.contents, &self.room_id, move |room| {
                (room.state_value(key), Default::default())
            })
            .await
            .unwrap()
        }

        /// Let `secs` seconds of (paused) time pass, a second at a time,
        /// until `done`; fail if they pass first.
        async fn until(&self, secs: u32, what: &str, done: impl Fn(&Fixture) -> bool) {
            for _ in 0..secs {
                if done(self) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            assert!(done(self), "never happened: {what}");
        }
    }

    /// Cell `a`'s source in notebook JSON as a file holds it: a string, or
    /// an array of lines.
    fn source_of_a(content: &Value) -> String {
        match &content["cells"][0]["source"] {
            Value::String(s) => s.clone(),
            lines => lines
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect(),
        }
    }

    /// Saves fail, the client leaves, and the room gives up after
    /// `GIVE_UP_AFTER` failures: its edits are in a recovery copy.
    #[tokio::test(start_paused = true)]
    async fn a_room_whose_saves_keep_failing_keeps_a_recovery_copy() {
        let fx = Fixture::new(false).await;
        fx.fail.store(true, Ordering::SeqCst);
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("typed here").await;
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(2000, "a recovery copy", |fx| !fx.copies().is_empty())
            .await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "save_failed");
        assert!(copies[0].detail.contains("disk full"), "{:?}", copies[0]);
        let (_, content) = recovery::read(&fx.store(), &copies[0].id)
            .unwrap()
            .unwrap();
        let nb = Notebook::from_value(content.clone()).expect("the copy is a notebook");
        assert_eq!(nb.cells.len(), 1);
        assert_eq!(source_of_a(&content), "typed here");
        // The file still has what it had.
        let on_disk = std::fs::read_to_string(fx.dir.path().join("a.ipynb")).unwrap();
        assert!(on_disk.contains("keep") && !on_disk.contains("typed here"));
        fx.until(100, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
    }

    /// The file is deleted under a room with unsaved edits.
    #[tokio::test(start_paused = true)]
    async fn a_room_whose_file_is_gone_keeps_a_recovery_copy() {
        let fx = Fixture::new(false).await;
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("typed here").await;
        std::fs::remove_file(fx.dir.path().join("a.ipynb")).unwrap();
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(2000, "a recovery copy", |fx| !fx.copies().is_empty())
            .await;
        let copies = fx.copies();
        assert_eq!(copies[0].reason, "file_gone");
        let (_, content) = recovery::read(&fx.store(), &copies[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(source_of_a(&content), "typed here");
        fx.until(100, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        assert!(!fx.dir.path().join("a.ipynb").exists());
    }

    /// A copy that can't be written keeps the room open; once it can, the
    /// room writes it and retires.
    #[tokio::test(start_paused = true)]
    async fn a_room_that_cannot_keep_its_copy_stays_open() {
        let fx = Fixture::new(true).await;
        fx.fail.store(true, Ordering::SeqCst);
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("typed here").await;
        tx.send(Cmd::Leave(1)).unwrap();
        // Far past the point where it would have given up.
        tokio::time::sleep(Duration::from_secs(1500)).await;
        assert_eq!(
            fx.collab.open_rooms(),
            1,
            "the room retired, losing its edits"
        );
        assert!(!tx.is_closed());
        // The blocked dir is cleared: the next cleanup tick succeeds.
        std::fs::remove_file(fx.recovery_dir()).unwrap();
        fx.until(300, "a recovery copy", |fx| !fx.copies().is_empty())
            .await;
        assert_eq!(fx.copies()[0].reason, "save_failed");
        fx.until(100, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
    }

    /// Without a recovery dir a room closes as it did before.
    #[tokio::test(start_paused = true)]
    async fn a_room_without_a_recovery_dir_still_gives_up() {
        let mut fx = Fixture::new(false).await;
        fx.collab = Arc::new(
            Collab::new(FileIds::in_memory(), None).with_cleanup_delay(Duration::from_secs(1)),
        );
        fx.room_id = notebook_room_id(&fx.collab, &fx.contents, "a.ipynb")
            .await
            .unwrap();
        fx.fail.store(true, Ordering::SeqCst);
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("typed here").await;
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(2000, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        // The fixture's server made the dir; nothing is in it.
        assert_eq!(std::fs::read_dir(fx.recovery_dir()).unwrap().count(), 0);
        assert!(std::fs::read_to_string(fx.dir.path().join("a.ipynb"))
            .unwrap()
            .contains("keep"));
    }

    /// The room and the disk both changed cell `a`: the disk wins it, and
    /// the room's version is kept and named in `state.recovery`.
    #[tokio::test(start_paused = true)]
    async fn a_merge_conflict_keeps_the_rooms_version() {
        let fx = Fixture::new(false).await;
        let (_tx, _rx) = fx.join(1).await;
        fx.type_into_a("room wrote this").await;
        std::fs::write(
            fx.dir.path().join("a.ipynb"),
            NB.replace("keep", "disk wrote this"),
        )
        .unwrap();
        fx.until(30, "a recovery copy", |fx| !fx.copies().is_empty())
            .await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "merge_conflict");
        assert_eq!(copies[0].cells, ["a"]);
        assert_eq!(copies[0].detail, "cell a");
        let (_, content) = recovery::read(&fx.store(), &copies[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(source_of_a(&content), "room wrote this");
        let named = fx.state("recovery").await.expect("state.recovery is set");
        assert_eq!(named["id"], copies[0].id.as_str());
        assert_eq!(named["reason"], "merge_conflict");
        assert_eq!(named["cells"], json!(["a"]));
        // The disk won the cell in the room.
        let nb = with_notebook(&fx.collab, &fx.contents, &fx.room_id, |room| {
            (room.notebook().unwrap().unwrap(), Default::default())
        })
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(&nb).unwrap()["cells"][0]["source"],
            "disk wrote this"
        );
    }

    /// A merge with no conflict keeps no copy.
    #[tokio::test(start_paused = true)]
    async fn a_merge_without_a_conflict_keeps_no_copy() {
        let fx = Fixture::new(false).await;
        let (_tx, _rx) = fx.join(1).await;
        fx.type_into_a("room wrote this").await;
        // The disk adds a cell instead.
        std::fs::write(
            fx.dir.path().join("a.ipynb"),
            NB.replace(
                "}],\"metadata\"",
                "},{\"cell_type\":\"markdown\",\"id\":\"b\",\"metadata\":{},\"source\":\"new\"}],\"metadata\"",
            ),
        )
        .unwrap();
        fx.until(30, "the room saved the merge", |fx| {
            std::fs::read_to_string(fx.dir.path().join("a.ipynb"))
                .unwrap()
                .contains("room wrote this")
        })
        .await;
        assert!(fx.copies().is_empty());
        assert_eq!(fx.state("recovery").await, None);
    }

    /// `state.save` follows the saves, and neither it nor the other state
    /// keys reach the file.
    #[tokio::test(start_paused = true)]
    async fn save_state_follows_the_saves_and_stays_out_of_the_file() {
        let fx = Fixture::new(false).await;
        assert_eq!(fx.state("save").await, Some(json!("saved")));
        let (_tx, _rx) = fx.join(1).await;
        fx.fail.store(true, Ordering::SeqCst);
        fx.type_into_a("typed here").await;
        let failed = || async { fx.state("save").await == Some(json!("failed")) };
        for _ in 0..30 {
            if failed().await {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(failed().await, "state.save never failed");
        let error = fx.state("save_error").await.unwrap();
        assert!(error.as_str().unwrap().contains("disk full"), "{error}");
        fx.fail.store(false, Ordering::SeqCst);
        for _ in 0..200 {
            if fx.state("save").await == Some(json!("saved")) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert_eq!(fx.state("save").await, Some(json!("saved")));
        assert_eq!(fx.state("save_error").await, None);
        let on_disk = std::fs::read_to_string(fx.dir.path().join("a.ipynb")).unwrap();
        assert!(on_disk.contains("typed here"), "the save happened");
        for key in ["save", "save_error", "recovery", "dirty", "hash", "state"] {
            assert!(
                !on_disk.contains(&format!("\"{key}\"")),
                "{key} reached the file: {on_disk}"
            );
        }
    }

    /// A room that opens on a path with recovery copies names the newest;
    /// discarding it clears the name.
    #[tokio::test(start_paused = true)]
    async fn opening_a_room_names_the_newest_copy_until_it_is_discarded() {
        let fx = Fixture::new(false).await;
        let nb = Notebook::from_value(serde_json::from_str(NB).unwrap()).unwrap();
        let store = fx.store();
        let write = |path: &str, reason| {
            recovery::write(&store, path, &nb, reason, "x", &[])
                .unwrap()
                .0
        };
        let old = write("a.ipynb", Reason::SaveFailed);
        let (new, _) = recovery::write(
            &store,
            "a.ipynb",
            &nb,
            Reason::MergeConflict,
            "cell a",
            &["a".to_string()],
        )
        .unwrap();
        write("other.ipynb", Reason::FileGone);
        let named = fx.state("recovery").await.expect("state.recovery is set");
        assert_eq!(named["id"], new.id.as_str());
        assert_eq!(named["reason"], "merge_conflict");
        assert_eq!(named["cells"], json!(["a"]));
        // Discarding the named one names the next newest of the path, and
        // discarding another leaves that; with none left, none is named.
        recovery::discard(&store, &new.id).unwrap();
        fx.collab.forget_recovery(&new.id, Some("a.ipynb")).await;
        let named = fx.state("recovery").await.expect("the older copy");
        assert_eq!(named["id"], old.id.as_str());
        fx.collab.forget_recovery(&new.id, Some("a.ipynb")).await;
        assert!(fx.state("recovery").await.is_some());
        recovery::discard(&store, &old.id).unwrap();
        fx.collab.forget_recovery(&old.id, Some("a.ipynb")).await;
        assert_eq!(fx.state("recovery").await, None);
    }

    impl Fixture {
        /// Edit the room on the server's side.
        async fn edit(&self, f: impl FnOnce(&mut Room) + Send + 'static) {
            with_notebook(&self.collab, &self.contents, &self.room_id, move |room| {
                f(room);
                let fx = ganymede_documents::Effects {
                    changed: true,
                    ..Default::default()
                };
                ((), fx)
            })
            .await
            .unwrap();
        }

        /// The notebook in a room and its file, with no task running them.
        async fn bare(&self) -> (Room, Disk) {
            let loaded = load(self.contents.as_ref(), "a.ipynb").await;
            let (nb, mut disk) = loaded.unwrap_or_else(|e| panic!("{e}"));
            let mut room = Room::notebook_room(&nb, "a.ipynb");
            room.saved(0, &disk.hash);
            disk.recovery = Some(self.collab.clone());
            (room, disk)
        }

        async fn save(&self, room: &mut Room, disk: &mut Disk, force: bool) -> SaveStatus {
            save_room(self.contents.as_ref(), room, disk, &HashMap::new(), force).await
        }

        async fn reload(&self, room: &mut Room, disk: &mut Disk) -> Reloaded {
            reload(self.contents.as_ref(), room, disk, &HashMap::new()).await
        }

        fn write_file(&self, text: &str) {
            std::fs::write(self.dir.path().join("a.ipynb"), NB.replace("keep", text)).unwrap();
        }

        /// Let copies be written again.
        fn unblock(&self) {
            std::fs::remove_file(self.recovery_dir()).unwrap();
        }
    }

    fn type_a(room: &mut Room, text: &'static str) {
        room.edit(|txn, roots| {
            assert!(crate::room_ops::set_source(txn, roots, "a", text, "me"));
        });
    }

    /// Put something in the room that no notebook holds.
    fn spoil(room: &mut Room) {
        use ganymede_documents::yrs::Map;
        room.edit(|txn, roots| roots.meta.remove(txn, "nbformat"));
        assert!(room.notebook().unwrap().is_err() && room.is_dirty());
    }

    fn save_state(room: &Room) -> (Value, String) {
        let error = room.state_value("save_error");
        (
            room.state_value("save").unwrap(),
            error.map_or(String::new(), |e| e.as_str().unwrap().to_string()),
        )
    }

    /// A conflict whose copy can't be written keeps `save` failed after the
    /// merge and after good saves, until the copy is written.
    #[tokio::test]
    async fn edits_lost_without_a_copy_stay_failed_until_they_have_one() {
        let fx = Fixture::new(true).await;
        let (mut room, mut disk) = fx.bare().await;
        type_a(&mut room, "room wrote this");
        fx.write_file("disk wrote this");
        assert!(fx.reload(&mut room, &mut disk).await.lost);
        let failed = |room: &Room| {
            let (save, error) = save_state(room);
            assert_eq!(save, json!("failed"));
            assert!(
                error.contains("cell a") && error.contains("not be kept"),
                "{error}"
            );
        };
        failed(&room);
        // A reload and a good save don't clear it.
        fx.write_file("disk wrote more");
        fx.reload(&mut room, &mut disk).await;
        failed(&room);
        type_a(&mut room, "a later edit");
        assert_eq!(
            fx.save(&mut room, &mut disk, false).await,
            SaveStatus::Success
        );
        failed(&room);
        // Nor may the room close, with or without other edits.
        assert!(!keep_lost(&mut room, &mut disk, &HashMap::new()).await);
        // Once a copy can be written, the next save writes it.
        fx.unblock();
        type_a(&mut room, "another edit");
        assert_eq!(
            fx.save(&mut room, &mut disk, false).await,
            SaveStatus::Success
        );
        assert_eq!(save_state(&room), (json!("saved"), String::new()));
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "merge_conflict");
        let (_, content) = recovery::read(&fx.store(), &copies[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(source_of_a(&content), "room wrote this");
        assert_eq!(
            room.state_value("recovery").unwrap()["id"],
            copies[0].id.as_str()
        );
    }

    /// A person who saves the room as it is has seen the failure.
    #[tokio::test]
    async fn a_clients_own_save_clears_edits_lost_without_a_copy() {
        let fx = Fixture::new(true).await;
        let (mut room, mut disk) = fx.bare().await;
        type_a(&mut room, "room wrote this");
        fx.write_file("disk wrote this");
        fx.reload(&mut room, &mut disk).await;
        assert_eq!(save_state(&room).0, json!("failed"));
        assert_eq!(
            fx.save(&mut room, &mut disk, true).await,
            SaveStatus::Success
        );
        assert_eq!(save_state(&room), (json!("saved"), String::new()));
        assert!(keep_lost(&mut room, &mut disk, &HashMap::new()).await);
    }

    /// The save whose own merge lost the edits doesn't clear them.
    #[tokio::test]
    async fn a_save_that_loses_edits_does_not_clear_them() {
        let fx = Fixture::new(true).await;
        let (mut room, mut disk) = fx.bare().await;
        type_a(&mut room, "room wrote this");
        fx.write_file("disk wrote this");
        assert_eq!(
            fx.save(&mut room, &mut disk, true).await,
            SaveStatus::Failed
        );
        assert_eq!(save_state(&room).0, json!("failed"));
    }

    /// Lost edits hold an empty room open until their copy is written.
    #[tokio::test(start_paused = true)]
    async fn a_room_with_lost_edits_stays_open_until_they_have_a_copy() {
        let fx = Fixture::new(true).await;
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("room wrote this").await;
        fx.write_file("disk wrote this");
        fx.until_state("save", json!("failed")).await;
        tx.send(Cmd::Leave(1)).unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(fx.collab.open_rooms(), 1);
        fx.unblock();
        fx.until(100, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        assert_eq!(fx.copies()[0].reason, "merge_conflict");
    }

    /// The file takes on what the room couldn't save: the room is clean, so
    /// its save state is `saved` again.
    #[tokio::test]
    async fn a_reload_that_leaves_the_room_clean_says_saved() {
        let fx = Fixture::new(false).await;
        let (mut room, mut disk) = fx.bare().await;
        fx.fail.store(true, Ordering::SeqCst);
        // Without the agent's name that `type_a` leaves in the metadata.
        room.edit(|txn, roots| {
            use ganymede_documents::{schema, yrs};
            let a = schema::find_cell(txn, roots, "a").unwrap();
            let Some(yrs::Out::YText(source)) = yrs::Map::get(&a, txn, "source") else {
                panic!("no source")
            };
            schema::set_text(txn, &source, "same");
        });
        fx.save(&mut room, &mut disk, false).await;
        let (save, error) = save_state(&room);
        assert_eq!(save, json!("failed"));
        assert!(error.contains("disk full"), "{error}");
        fx.write_file("same");
        assert!(!fx.reload(&mut room, &mut disk).await.lost);
        assert!(!room.is_dirty());
        assert_eq!(save_state(&room), (json!("saved"), String::new()));
        assert!(fx.copies().is_empty());
    }

    /// The disk replaces a room that isn't a notebook: there is nothing to
    /// keep, and clients are told so.
    #[tokio::test]
    async fn a_replaced_room_that_is_no_notebook_says_so() {
        let fx = Fixture::new(false).await;
        let (mut room, mut disk) = fx.bare().await;
        spoil(&mut room);
        fx.write_file("disk wrote this");
        assert!(fx.reload(&mut room, &mut disk).await.lost);
        assert!(room.notebook().unwrap().is_ok(), "the file's version");
        let (save, error) = save_state(&room);
        assert_eq!(save, json!("failed"));
        assert!(error.contains("not a notebook"), "{error}");
        assert!(fx.copies().is_empty());
        // It holds no room open.
        assert!(keep_lost(&mut room, &mut disk, &HashMap::new()).await);
    }

    /// No copy of a room that isn't a notebook is possible, so it closes
    /// when its saves keep failing, as without recovery copies.
    #[tokio::test(start_paused = true)]
    async fn a_room_that_is_no_notebook_still_gives_up() {
        let fx = Fixture::new(false).await;
        let (tx, _rx) = fx.join(1).await;
        fx.edit(spoil).await;
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(2000, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        assert!(fx.copies().is_empty());
    }

    /// The server stops while a save fails: the edits get a copy.
    #[tokio::test(start_paused = true)]
    async fn stopping_keeps_a_copy_of_edits_that_cannot_be_saved() {
        let fx = Fixture::new(false).await;
        let (_tx, _rx) = fx.join(1).await;
        fx.fail.store(true, Ordering::SeqCst);
        fx.type_into_a("typed here").await;
        fx.collab.flush_all().await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "save_failed");
        assert!(copies[0].detail.contains("disk full"), "{:?}", copies[0]);
        let (_, content) = recovery::read(&fx.store(), &copies[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(source_of_a(&content), "typed here");
    }

    /// The server stops with edits in a read-only room, or with none.
    #[tokio::test(start_paused = true)]
    async fn stopping_keeps_a_copy_of_a_read_only_rooms_edits() {
        let fx = Fixture::new(false).await;
        let (_tx, _rx) = fx.join(1).await;
        fx.collab.flush_all().await;
        assert!(fx.copies().is_empty(), "nothing unsaved");
        fx.edit(|room| {
            type_a(room, "typed here");
            room.set_read_only(true);
        })
        .await;
        fx.collab.flush_all().await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "read_only");
    }

    /// An empty read-only room with edits closes, with a copy of them.
    #[tokio::test(start_paused = true)]
    async fn a_read_only_room_keeps_a_copy_before_closing() {
        let fx = Fixture::new(false).await;
        let (tx, _rx) = fx.join(1).await;
        fx.edit(|room| {
            type_a(room, "typed here");
            room.set_read_only(true);
        })
        .await;
        fx.until_state("save", json!("failed")).await;
        assert_eq!(
            fx.state("save_error").await,
            Some(json!("another server owns the file"))
        );
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(100, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "read_only");
    }

    /// The cleanup finds the file gone before the room's own look does. It
    /// doesn't drop the file's id, as the latter would.
    #[tokio::test(start_paused = true)]
    async fn a_cleanup_that_finds_the_file_gone_keeps_a_copy() {
        let fx = Fixture::new(false).await;
        let (tx, _rx) = fx.join(1).await;
        fx.vanish.store(true, Ordering::SeqCst);
        fx.fail.store(true, Ordering::SeqCst);
        fx.type_into_a("typed here").await;
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(10, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
        let copies = fx.copies();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].reason, "file_gone");
        let id = fx.room_id.rsplit(':').next().unwrap();
        assert_eq!(fx.collab.path_of(id).await.as_deref(), Some("a.ipynb"));
    }

    /// A close that a client's arrival stopped makes no second copy of the
    /// same content when it comes round again, and one of new edits.
    #[tokio::test]
    async fn closing_again_keeps_no_second_copy_of_the_same_edits() {
        let fx = Fixture::new(false).await;
        let (mut room, disk) = fx.bare().await;
        let clients = HashMap::new();
        let mut at = None;
        for (text, copies) in [("one", 1), ("one", 1), ("two", 2), ("two", 2)] {
            type_a(&mut room, text);
            let reason = Reason::SaveFailed;
            assert!(kept_before_closing(&mut room, &disk, &clients, &mut at, reason).await);
            assert_eq!(fx.copies().len(), copies, "{text}");
        }
    }

    /// A copy pushed out by the limits is no longer named by any room.
    #[tokio::test(start_paused = true)]
    async fn a_room_stops_naming_a_copy_that_is_pruned() {
        let fx = Fixture::new(false).await;
        let (room, disk) = fx.bare().await;
        let nb = room.notebook().unwrap().unwrap();
        let keep = || keep_copy(&disk, nb.clone(), Reason::SaveFailed, "".into(), vec![]);
        let first = keep().await.unwrap().unwrap();
        let named = fx.state("recovery").await.unwrap();
        assert_eq!(named["id"], first.id.as_str());
        let mut newest = first;
        for _ in 0..recovery::MAX_PER_PATH {
            newest = keep().await.unwrap().unwrap();
        }
        let named = fx.state("recovery").await.unwrap();
        assert_eq!(named["id"], newest.id.as_str());
    }

    /// Two servers' rooms on the same relative path, sharing a recovery
    /// dir: each is offered its own copies only.
    #[tokio::test(start_paused = true)]
    async fn servers_on_two_roots_offer_only_their_own_copies() {
        let data = tempfile::tempdir().unwrap();
        let nb = Notebook::from_value(serde_json::from_str(NB).unwrap()).unwrap();
        let mut servers = Vec::new();
        for _ in 0..2 {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("a.ipynb"), NB).unwrap();
            let contents: Arc<dyn Contents> =
                Arc::new(FsContents::new(dir.path(), Default::default()).unwrap());
            let collab = Collab::new(FileIds::in_memory(), Some(dir.path().to_path_buf()))
                .with_recovery_dir(data.path().join("recovery"));
            servers.push((dir, contents, Arc::new(collab)));
        }
        let store = servers[0].2.recovery.clone().unwrap();
        let (copy, _) =
            recovery::write(&store, "a.ipynb", &nb, Reason::SaveFailed, "", &[]).unwrap();
        let mut named = Vec::new();
        for (_, contents, collab) in &servers {
            let id = notebook_room_id(collab, contents, "a.ipynb").await.unwrap();
            let state = with_notebook(collab, contents, &id, |room| {
                (room.state_value("recovery"), Default::default())
            });
            named.push(state.await.unwrap().map(|r| r["id"].clone()));
        }
        assert_eq!(named, [Some(json!(copy.id)), None]);
    }

    /// Two servers on one data dir, one rooted at a folder and one at a
    /// project inside it (#945): the outer one lists, reads and discards
    /// the inner one's copy at its path relative to the outer root, and
    /// the inner one doesn't see a third root's copies.
    #[test]
    fn copies_are_seen_from_every_root_that_holds_the_file() {
        let data = tempfile::tempdir().unwrap();
        let outer_root = tempfile::tempdir().unwrap();
        let inner_dir = outer_root.path().join("proj");
        std::fs::create_dir(&inner_dir).unwrap();
        let far = tempfile::tempdir().unwrap();
        let collab = |root: PathBuf| {
            Collab::new(FileIds::in_memory(), Some(root))
                .with_recovery_dir(data.path().join("recovery"))
        };
        let (outer, inner, elsewhere) = (
            collab(outer_root.path().to_path_buf()),
            collab(inner_dir),
            collab(far.path().to_path_buf()),
        );
        let nb = Notebook::from_value(serde_json::from_str(NB).unwrap()).unwrap();
        let inner_store = inner.recovery.clone().unwrap();
        let mine = recovery::write(
            &inner_store,
            "a.ipynb",
            &nb,
            Reason::MergeConflict,
            "cell a",
            &["a".to_string()],
        )
        .unwrap()
        .0;
        let far_store = elsewhere.recovery.clone().unwrap();
        recovery::write(&far_store, "a.ipynb", &nb, Reason::SaveFailed, "", &[]).unwrap();
        // The outer server sees the inner one's copy at proj/a.ipynb.
        let outer_store = outer.recovery.clone().unwrap();
        let listed = recovery::list(&outer_store, "proj/a.ipynb").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, mine.id);
        assert_eq!(listed[0].path, "proj/a.ipynb");
        let (meta, _) = recovery::read(&outer_store, &mine.id).unwrap().unwrap();
        assert_eq!(meta.path, "proj/a.ipynb");
        // The inner one sees it at its own path, not the far root's
        // copy of the same path.
        let at_inner = recovery::list(&inner_store, "a.ipynb").unwrap();
        assert_eq!(at_inner.len(), 1);
        assert_eq!(at_inner[0].id, mine.id);
        // Discarding from the outer root removes it everywhere.
        assert!(recovery::discard(&outer_store, &mine.id).unwrap());
        assert!(recovery::read(&inner_store, &mine.id).unwrap().is_none());
        assert_eq!(recovery::list(&inner_store, "a.ipynb").unwrap(), []);
    }

    /// A recovery dir that can't be made leaves the server without one, so
    /// its rooms close as before instead of waiting for a copy.
    #[tokio::test(start_paused = true)]
    async fn a_recovery_dir_that_cannot_be_made_is_done_without() {
        let mut fx = Fixture::new(false).await;
        let blocked = fx.data.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        fx.collab = Arc::new(
            Collab::new(FileIds::in_memory(), None)
                .with_recovery_dir(blocked.join("recovery"))
                .with_cleanup_delay(Duration::from_secs(1)),
        );
        assert!(fx.collab.recovery.is_none());
        fx.room_id = notebook_room_id(&fx.collab, &fx.contents, "a.ipynb")
            .await
            .unwrap();
        fx.fail.store(true, Ordering::SeqCst);
        let (tx, _rx) = fx.join(1).await;
        fx.type_into_a("typed here").await;
        tx.send(Cmd::Leave(1)).unwrap();
        fx.until(2000, "the room retired", |fx| fx.collab.open_rooms() == 0)
            .await;
    }
}
