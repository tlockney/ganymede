//! A room: one live document and the clients connected to it, as a state
//! machine with no I/O.
//!
//! The host (an HTTP server, or an app through `ganymede-ffi`) owns the
//! sockets and the files. It feeds the room connection events and the
//! bytes each client sends, and carries out what comes back: frames to
//! send, and save requests. Loading and saving go through
//! [`Room::notebook`], [`Room::reload`] and [`Room::saved`].
//!
//! The wire protocol is y-websocket's, as `pycrdt-websocket` and
//! `jupyter-collaboration` speak it: each frame is one message whose first
//! variable-length integer is its type, `0` sync (step 1, step 2, update),
//! `1` awareness, `3` awareness query, and `jupyter-collaboration`'s `2`,
//! which there means a raw string message (`"save"` requests and their JSON
//! replies) rather than y-protocols' auth.

use crate::schema::{self, Roots, SchemaError};
use ganymede_nbformat::Notebook;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use yrs::encoding::read::{Cursor, Read};
use yrs::encoding::write::Write;
use yrs::sync::{Awareness, Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{ClientID, Doc, Map, ReadTxn, StateVector, Transact, Update};

/// A connection, numbered by the host.
pub type ConnId = u64;

/// `jupyter-collaboration`'s raw message type.
pub const MSG_RAW: u32 = 2;

/// Who a frame goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    One(ConnId),
    /// Everyone but this connection: the one the change came from.
    Others(ConnId),
    All,
}

/// What the host should do after a call.
#[derive(Debug, Default)]
pub struct Effects {
    /// Binary websocket frames to send, in order.
    pub send: Vec<(Target, Vec<u8>)>,
    /// A client asked to save now (`jupyter-collaboration`'s save
    /// message); answer with [`Room::save_reply`].
    pub save: Option<(ConnId, u64)>,
    /// The content changed, so the document needs saving.
    pub changed: bool,
}

impl Effects {
    fn merge(&mut self, other: Effects) {
        self.send.extend(other.send);
        self.save = self.save.or(other.save);
        self.changed |= other.changed;
    }
}

/// How a save went, for [`Room::save_reply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    Success,
    Skipped,
    Failed,
}

/// Errors from [`Room::receive`]. The host should log them and may close
/// the connection; the room stays usable.
#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("bad message: {0}")]
    Decode(#[from] yrs::encoding::read::Error),
    #[error("bad awareness update: {0}")]
    Awareness(#[from] yrs::sync::awareness::Error),
}

const OBSERVER: &str = "ganymede-room";

/// One shared document.
pub struct Room {
    awareness: Awareness,
    /// `None` for a transient room (awareness only, such as JupyterLab's
    /// `JupyterLab:globalAwareness`).
    roots: Option<Roots>,
    /// Updates committed since the last drain, from the doc's observer.
    updates: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Awareness client ids each connection introduced, removed when it
    /// goes.
    conns: HashMap<ConnId, HashSet<ClientID>>,
    /// Bumped on every content change; see [`Room::generation`].
    generation: u64,
    saved_generation: u64,
    /// The notebook as last loaded, for [`schema::restore_numbers`].
    baseline: Option<Value>,
    /// Set when a transaction touches `cells` or `meta` (not `state`,
    /// which clients write too: `document_id` on connect).
    content_changed: Arc<AtomicBool>,
}

impl Room {
    /// A room holding `notebook`, loaded deterministically (see
    /// [`schema::deterministic_update`]).
    pub fn notebook_room(notebook: &Notebook, path: &str) -> Self {
        let mut room = Self::new(true);
        room.baseline = Some(schema::notebook_value(notebook));
        let update = schema::deterministic_update(notebook);
        {
            let doc = room.awareness.doc();
            let mut txn = doc.transact_mut();
            if let Ok(u) = Update::decode_v1(&update) {
                let _ = txn.apply_update(u);
            }
            let roots = room.roots.as_ref().expect("notebook room has roots");
            roots.state.insert(&mut txn, "dirty", false);
            roots.state.insert(&mut txn, "path", path);
        }
        room.drain();
        room.content_changed.store(false, Ordering::Relaxed);
        room
    }

    /// A room with no document content, for awareness only.
    pub fn transient() -> Self {
        Self::new(false)
    }

    fn new(notebook: bool) -> Self {
        let doc = Doc::new();
        let roots = notebook.then(|| Roots::get(&doc));
        let content_changed: Arc<AtomicBool> = Arc::default();
        if let Some(roots) = &roots {
            use yrs::DeepObservable;
            let flag = content_changed.clone();
            roots.cells.observe_deep(OBSERVER, move |_, _| {
                flag.store(true, Ordering::Relaxed);
            });
            let flag = content_changed.clone();
            roots.meta.observe_deep(OBSERVER, move |_, _| {
                flag.store(true, Ordering::Relaxed);
            });
        }
        let updates: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let sink = updates.clone();
        doc.observe_update_v1(OBSERVER, move |_, e| {
            sink.lock().expect("update sink").push(e.update.clone());
        })
        .expect("a new doc has no open transaction");
        Room {
            awareness: Awareness::new(doc),
            roots,
            updates,
            conns: HashMap::new(),
            generation: 0,
            saved_generation: 0,
            baseline: None,
            content_changed,
        }
    }

    /// The underlying document.
    pub fn doc(&self) -> &Doc {
        self.awareness.doc()
    }

    /// Number of connected clients.
    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    /// A counter that moves on every content change. A host that saves in
    /// the background reads it before taking [`Room::notebook`] and passes
    /// it to [`Room::saved`], so edits made during the save keep the
    /// document dirty.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Content changed since the last save or load.
    pub fn is_dirty(&self) -> bool {
        self.generation != self.saved_generation
    }

    /// A client connected: send it sync step 1 and who else is here.
    pub fn connect(&mut self, conn: ConnId) -> Effects {
        self.conns.entry(conn).or_default();
        let mut fx = Effects::default();
        let sv = self.doc().transact().state_vector();
        fx.send.push((
            Target::One(conn),
            Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1(),
        ));
        if let Ok(update) = self.awareness.update() {
            if !update.clients.is_empty() {
                fx.send
                    .push((Target::One(conn), Message::Awareness(update).encode_v1()));
            }
        }
        fx
    }

    /// A client went away: drop its awareness states and tell the others.
    pub fn disconnect(&mut self, conn: ConnId) -> Effects {
        let mut fx = Effects::default();
        let Some(clients) = self.conns.remove(&conn) else {
            return fx;
        };
        let clients: Vec<ClientID> = clients.into_iter().collect();
        for id in &clients {
            self.awareness.remove_state(*id);
        }
        if let Ok(update) = self.awareness.update_with_clients(clients.iter().copied()) {
            if !update.clients.is_empty() {
                fx.send
                    .push((Target::All, Message::Awareness(update).encode_v1()));
            }
        }
        fx
    }

    /// A frame from a client.
    pub fn receive(&mut self, conn: ConnId, data: &[u8]) -> Result<Effects, RoomError> {
        let mut fx = Effects::default();
        if data.is_empty() {
            return Ok(fx);
        }
        let mut cursor = Cursor::new(data);
        if cursor.read_var::<u32>()? == MSG_RAW {
            let text = cursor.read_string()?;
            if text == "save" {
                let id: u64 = cursor.read_var()?;
                fx.save = Some((conn, id));
            }
            return Ok(fx);
        }
        let mut decoder = DecoderV1::new(Cursor::new(data));
        let reader = MessageReader::new(&mut decoder);
        for message in reader {
            fx.merge(self.handle(conn, message?)?);
        }
        Ok(fx)
    }

    fn handle(&mut self, conn: ConnId, message: Message) -> Result<Effects, RoomError> {
        let mut fx = Effects::default();
        match message {
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                let update = self.doc().transact().encode_state_as_update_v1(&sv);
                fx.send.push((
                    Target::One(conn),
                    Message::Sync(SyncMessage::SyncStep2(update)).encode_v1(),
                ));
            }
            Message::Sync(SyncMessage::SyncStep2(bytes))
            | Message::Sync(SyncMessage::Update(bytes)) => {
                let applied = Update::decode_v1(&bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|u| {
                        self.doc()
                            .transact_mut()
                            .apply_update(u)
                            .map_err(|e| e.to_string())
                    });
                if let Err(e) = applied {
                    // jupyter-collaboration tells the client, which offers
                    // to reload.
                    tracing::warn!("update from client {conn} not applied: {e}");
                    fx.send
                        .push((Target::One(conn), raw_message(&json!({"type": "conflict"}))));
                }
                let updates = self.drain();
                if self.content_changed.swap(false, Ordering::Relaxed) {
                    self.generation += 1;
                    fx.changed = true;
                }
                if !updates.is_empty() {
                    for u in updates {
                        fx.send.push((
                            Target::Others(conn),
                            Message::Sync(SyncMessage::Update(u)).encode_v1(),
                        ));
                    }
                }
            }
            Message::Awareness(update) => {
                let ids: Vec<ClientID> = update.clients.keys().copied().collect();
                let encoded = Message::Awareness(update.clone()).encode_v1();
                if let Some(summary) = self.awareness.apply_update_summary(update)? {
                    let mine = self.conns.entry(conn).or_default();
                    mine.extend(summary.added.iter().copied());
                    for id in &summary.removed {
                        mine.remove(id);
                    }
                }
                if !ids.is_empty() {
                    fx.send.push((Target::Others(conn), encoded));
                }
            }
            Message::AwarenessQuery => {
                let update = self.awareness.update()?;
                fx.send
                    .push((Target::One(conn), Message::Awareness(update).encode_v1()));
            }
            Message::Auth(_) | Message::Custom(..) => {}
        }
        Ok(fx)
    }

    fn drain(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.updates.lock().expect("update sink"))
    }

    /// Frames for updates the server made itself, to everyone.
    fn broadcast_own(&self) -> Effects {
        let mut fx = Effects::default();
        for u in self.drain() {
            fx.send.push((
                Target::All,
                Message::Sync(SyncMessage::Update(u)).encode_v1(),
            ));
        }
        fx
    }

    /// The document as a notebook, for saving. `None` for a transient room.
    pub fn notebook(&self) -> Option<Result<Notebook, SchemaError>> {
        let roots = self.roots.as_ref()?;
        Some(schema::get_restoring(
            &self.doc().transact(),
            roots,
            self.baseline.as_ref(),
        ))
    }

    /// The host wrote the document as of `generation` to disk, and the
    /// file now hashes to `hash` (contents' sha256). Sets `state.hash`,
    /// which JupyterLab watches to learn of saves, and `state.dirty`.
    pub fn saved(&mut self, generation: u64, hash: &str) -> Effects {
        self.saved_generation = self.saved_generation.max(generation);
        self.set_state(hash)
    }

    fn set_state(&mut self, hash: &str) -> Effects {
        if let Some(roots) = &self.roots {
            let dirty = self.is_dirty();
            let mut txn = self.awareness.doc().transact_mut();
            roots.state.insert(&mut txn, "dirty", dirty);
            if roots
                .state
                .get(&txn, "hash")
                .map(|o| o.to_string(&txn))
                .as_deref()
                != Some(hash)
            {
                roots.state.insert(&mut txn, "hash", hash);
            }
        }
        self.broadcast_own()
    }

    /// The file changed on disk: make the document match it, granularly
    /// (see [`schema::set`]), and mark it clean at `hash`.
    pub fn reload(&mut self, notebook: &Notebook, hash: &str) -> Effects {
        let Some(roots) = self.roots.clone() else {
            return Effects::default();
        };
        {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set(&mut txn, &roots, notebook);
        }
        self.content_changed.store(false, Ordering::Relaxed);
        self.generation += 1;
        self.saved_generation = self.generation;
        self.baseline = Some(schema::notebook_value(notebook));
        let mut fx = self.broadcast_own();
        fx.merge(self.set_state(hash));
        fx
    }

    /// The file was renamed.
    pub fn set_path(&mut self, path: &str) -> Effects {
        if let Some(roots) = &self.roots {
            let mut txn = self.awareness.doc().transact_mut();
            roots.state.insert(&mut txn, "path", path);
        }
        self.broadcast_own()
    }

    /// Edit the document on the server's side, as a kernel writing outputs
    /// will. The closure gets a transaction and the root types; the change
    /// goes to every client and makes the document dirty.
    pub fn edit<R>(
        &mut self,
        f: impl FnOnce(&mut yrs::TransactionMut, &Roots) -> R,
    ) -> (R, Effects) {
        let roots = self.roots.clone().unwrap_or_else(|| Roots::get(self.doc()));
        let r = {
            let mut txn = self.awareness.doc().transact_mut();
            f(&mut txn, &roots)
        };
        let mut fx = self.broadcast_own();
        if self.content_changed.swap(false, Ordering::Relaxed) {
            self.generation += 1;
            fx.changed = true;
        }
        (r, fx)
    }

    /// The reply to a client's save request.
    pub fn save_reply(id: u64, status: SaveStatus) -> Vec<u8> {
        let status = match status {
            SaveStatus::Success => "success",
            SaveStatus::Skipped => "skipped",
            SaveStatus::Failed => "failed",
        };
        raw_message(&json!({"type": "save", "responseTo": id, "status": status}))
    }

    /// The full state, for tests and for persisting a room.
    pub fn encode_state(&self) -> Vec<u8> {
        self.doc()
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }
}

/// `jupyter-collaboration`'s raw message: type 2 and a JSON string.
pub fn raw_message(value: &Value) -> Vec<u8> {
    let mut enc = EncoderV1::new();
    enc.write_var(MSG_RAW);
    enc.write_string(&value.to_string());
    enc.to_vec()
}
