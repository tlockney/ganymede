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

/// Hash a cell's type and source into `h`; whether it is a code cell.
fn hash_type_and_source<T: ReadTxn>(
    txn: &T,
    m: &yrs::MapRef,
    h: &mut std::collections::hash_map::DefaultHasher,
) -> bool {
    use std::hash::Hash;
    use yrs::{GetString, Out};
    let code = match m.get(txn, "cell_type") {
        Some(Out::Any(yrs::Any::String(t))) => {
            t.hash(h);
            &*t == "code"
        }
        _ => {
            "".hash(h);
            false
        }
    };
    match m.get(txn, "source") {
        Some(Out::YText(t)) => t.get_string(txn).hash(h),
        Some(Out::Any(yrs::Any::String(t))) => t.hash(h),
        Some(other) => schema::out_to_json(txn, &other).to_string().hash(h),
        None => {}
    }
    code
}

/// Whether a cell's `outputs` is now empty: missing, an empty `Any`
/// array, or an empty shared array.
fn outputs_empty<T: ReadTxn>(txn: &T, cell: &yrs::MapRef) -> bool {
    use yrs::{Array, Out};
    match cell.get(txn, "outputs") {
        None => true,
        Some(Out::YArray(a)) => a.len(txn) == 0,
        Some(Out::Any(yrs::Any::Array(items))) => items.is_empty(),
        Some(_) => false,
    }
}

/// The id of the cell in `cells` that `event` shows a client ran or
/// cleared, if it did, and whether it shows that by the count: the update
/// set its `execution_count` to another value (`true`), or left its
/// outputs empty (`false`; [`Room::receive`] counts that only for a cell
/// whose outputs weren't empty before). A cell added or replaced whole is
/// no such change, nor is writing the count it has, nor adding to
/// outputs, growing one, or replacing them with a non-empty list (a client
/// echoing a server run).
fn rerun_cell(
    txn: &yrs::TransactionMut,
    cells: &yrs::ArrayRef,
    event: &yrs::types::Event,
) -> Option<(String, bool)> {
    use yrs::types::{EntryChange, Event, PathSegment};
    use yrs::{Array, Out};
    let path = event.path();
    let Some(PathSegment::Index(index)) = path.front() else {
        return None;
    };
    let Some(Out::YMap(cell)) = cells.get(txn, *index) else {
        return None;
    };
    // (by the count, by emptied outputs)
    let (count, emptied) = match (path.get(1), event) {
        // A change under `outputs` counts only when it empties them.
        (Some(PathSegment::Key(key)), _) => {
            (false, &**key == "outputs" && outputs_empty(txn, &cell))
        }
        (None, Event::Map(e)) => {
            let keys = e.keys(txn);
            let count = match keys.get("execution_count") {
                Some(EntryChange::Updated(Out::Any(old), Out::Any(new))) => old != new,
                change => change.is_some(),
            };
            (
                count,
                keys.contains_key("outputs") && outputs_empty(txn, &cell),
            )
        }
        _ => (false, false),
    };
    if !count && !emptied {
        return None;
    }
    match cell.get(txn, "id") {
        Some(Out::Any(yrs::Any::String(id))) => Some((id.to_string(), count)),
        _ => None,
    }
}

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

/// Who last changed a cell's source or type, as the room saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The name the server knows the client by ([`Room::connect_as`]), the
    /// name the client gave in its awareness state (JupyterLab's
    /// `user.display_name` or `user.name`), the name the server's caller
    /// gave ([`Room::set_author`]), or a stand-in.
    pub name: String,
    /// The change came from a client connected to the room, such as a
    /// person's JupyterLab or Ganymede app, rather than from the server's
    /// own edits or the file.
    pub person: bool,
}

/// A name a client gave, made safe to show in a line of text: control
/// characters dropped, whitespace collapsed, at most 64 characters.
/// Empty when nothing is left.
pub fn clean_name(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    words
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_string()
}

/// How many deleted cells' authors a room keeps for undo.
const GONE_LIMIT: usize = 4096;

/// The author of the server's own edits ([`Room::edit`]) until its
/// caller names one ([`Room::set_author`]).
pub const SERVER: &str = "the server";

/// The stand-in name for a client that gave none.
pub const UNNAMED: &str = "a person";

/// The author of a change taken in from the file.
pub const FROM_FILE: &str = "another program (the file)";

fn from_file() -> Author {
    Author {
        name: FROM_FILE.to_string(),
        person: false,
    }
}

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
    /// Names the server vouches for, by connection ([`Room::connect_as`]),
    /// used for authorship before any awareness name.
    vouched: HashMap<ConnId, String>,
    /// Bumped on every content change; see [`Room::generation`].
    generation: u64,
    saved_generation: u64,
    /// The notebook as last loaded, for [`schema::restore_numbers`].
    baseline: Option<Value>,
    /// Set when a transaction touches `cells` or `meta` (not `state`,
    /// which clients write too: `document_id` on connect).
    content_changed: Arc<AtomicBool>,
    /// Ids of the cells [`rerun_cell`] found since this was last emptied,
    /// each `true` when its count changed, `false` when only its outputs
    /// were emptied, from the observer of `cells`. A client's update is
    /// emptied before and read after; the rest is ignored.
    rerun: Arc<Mutex<HashMap<String, bool>>>,
    /// Another process owns the file (#272): see [`Room::set_read_only`].
    read_only: bool,
    /// The depth of each nested shared type, by the id of the item holding
    /// it, and the deepest: measured when the room is loaded and again when
    /// an update hangs on one it doesn't know (the server's own edits add
    /// some), and kept up from each client update in between. See
    /// [`crate::depth`].
    depths: HashMap<(u64, u32), usize>,
    shared_depth: usize,
    /// Who last changed each cell's source or type, by cell id, while the
    /// room is open. Kept in memory only.
    authors: HashMap<String, Author>,
    /// Deleted cells' authors, with their hash, so a cell that comes back
    /// as it was (an undo) keeps its author.
    gone: HashMap<String, (u64, Author)>,
    /// When a cell's source or type, or the set of cells, last changed
    /// while the room was open, and who made the change. Kept in memory
    /// only.
    last_change: Option<(std::time::SystemTime, Author)>,
    /// Whether the last [`Room::edit`] changed cells and is credited to
    /// the server stand-in, so [`Room::set_author`] may name who it was
    /// for in `last_change` too.
    edit_unclaimed: bool,
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
            roots.state.insert(&mut txn, "save", "saved");
        }
        room.drain();
        room.content_changed.store(false, Ordering::Relaxed);
        room.measure_depths();
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
        let rerun: Arc<Mutex<HashMap<String, bool>>> = Arc::default();
        if let Some(roots) = &roots {
            use yrs::DeepObservable;
            let flag = content_changed.clone();
            let (ids, cells) = (rerun.clone(), roots.cells.clone());
            roots.cells.observe_deep(OBSERVER, move |txn, events| {
                flag.store(true, Ordering::Relaxed);
                let mut ids = ids.lock().expect("rerun cells");
                for (id, count) in events.iter().filter_map(|e| rerun_cell(txn, &cells, e)) {
                    *ids.entry(id).or_default() |= count;
                }
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
            vouched: HashMap::new(),
            generation: 0,
            saved_generation: 0,
            baseline: None,
            content_changed,
            rerun,
            read_only: false,
            depths: HashMap::new(),
            shared_depth: 0,
            authors: HashMap::new(),
            last_change: None,
            edit_unclaimed: false,
            gone: HashMap::new(),
        }
    }

    /// How deep the shared types `checked` creates would sit. An item in
    /// the document that the update hangs on is looked up in the measured
    /// depths; when one holding a shared type isn't there (the server added
    /// it since), the room measures again once. A sibling that isn't a
    /// shared type has no depth of its own, so the deepest the document
    /// goes stands in, and the result can overstate.
    fn type_depths(
        &mut self,
        checked: &crate::depth::Checked,
    ) -> Result<crate::depth::TypeDepths, crate::depth::Unresolved> {
        use crate::depth::LiveRef;
        let sv = self.doc().transact().state_vector();
        let mut remeasured = false;
        loop {
            let bound = self.shared_depth.max(crate::depth::SCHEMA_DEPTH);
            let mut type_missed = false;
            let depths = &self.depths;
            let result = checked.type_depths(&sv, bound, |r| match r {
                LiveRef::Type(c, k) => {
                    let d = depths.get(&(c, k)).copied();
                    type_missed |= d.is_none();
                    d
                }
                // A sibling that holds a shared type: the parent is one up.
                LiveRef::Beside(c, k) => depths.get(&(c, k)).map(|d| d - 1),
            });
            if type_missed && !remeasured {
                self.measure_depths();
                remeasured = true;
                continue;
            }
            return result;
        }
    }

    /// Measure the depth of every shared type in the document.
    fn measure_depths(&mut self) {
        let (depths, deepest) = crate::depth::shared_depths(&self.doc().transact());
        self.depths = depths;
        self.shared_depth = deepest;
    }

    /// The underlying document.
    pub fn doc(&self) -> &Doc {
        self.awareness.doc()
    }

    /// Number of connected clients.
    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    /// The connections and the names they go by (see `Room::client_name`),
    /// in connection order, for listing who is in the room.
    pub fn client_names(&self) -> Vec<(ConnId, String)> {
        let mut conns: Vec<ConnId> = self.conns.keys().copied().collect();
        conns.sort_unstable();
        conns
            .into_iter()
            .map(|conn| (conn, self.client_name(conn)))
            .collect()
    }

    /// When a cell's source or type, or the set of cells, last changed
    /// while the room was open, and who by; `None` if nothing has.
    pub fn last_change(&self) -> Option<(std::time::SystemTime, &Author)> {
        self.last_change.as_ref().map(|(at, by)| (*at, by))
    }

    /// A counter that moves on every content change. A host that saves in
    /// the background reads it before taking [`Room::notebook`] and passes
    /// it to [`Room::saved`], so edits made during the save keep the
    /// document dirty.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Make the room read-only, or writable again: another process owns
    /// the file. A read-only room takes the file's changes but refuses
    /// clients' edits, sending everyone the document as it was, and its
    /// own edits ([`Room::edit`]) don't make it dirty, so the host never
    /// saves it.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// See [`Room::set_read_only`].
    pub fn is_read_only(&self) -> bool {
        self.read_only
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

    /// [`Room::connect`], for a client whose name the server knows from
    /// how it signed in (a tailnet login, say). Its edits are credited to
    /// `name`, whatever its awareness state says.
    pub fn connect_as(&mut self, conn: ConnId, name: &str) -> Effects {
        let name = clean_name(name);
        if !name.is_empty() {
            self.vouched.insert(conn, name);
        }
        self.connect(conn)
    }

    /// A client went away: drop its awareness states and tell the others.
    pub fn disconnect(&mut self, conn: ConnId) -> Effects {
        self.vouched.remove(&conn);
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
                // Checked first, without applying: decoding values nested
                // deeper would overflow the stack, and so would deleting
                // shared types nested deeper, which can happen during the
                // apply itself.
                let checked = crate::depth::check(&bytes);
                let mut depths = None;
                let refused = if checked.malformed {
                    Some("an update the room can't read".to_string())
                } else if checked.too_deep {
                    Some(format!(
                        "values nested deeper than {} levels",
                        crate::depth::MAX_DEPTH
                    ))
                } else if checked.adds_types() {
                    match self.type_depths(&checked) {
                        Ok(d) if d.deepest <= crate::depth::MAX_SHARED_DEPTH => {
                            depths = Some(d);
                            None
                        }
                        Ok(_) => Some(format!(
                            "shared types nested deeper than {} levels",
                            crate::depth::MAX_SHARED_DEPTH
                        )),
                        Err(crate::depth::Unresolved::Missing) => {
                            Some("shared types hung on items the room doesn't have".to_string())
                        }
                    }
                } else {
                    None
                };
                // Each cell's source and type before, to tell which the
                // update changed.
                let before = (refused.is_none() && !self.read_only).then(|| self.prints());
                // The cells that had outputs: emptying a cell's outputs is
                // a run (or a clear) only when there was something to
                // empty, not a client writing back a server run's none.
                let had_outputs = before.as_ref().map(|_| self.cells_with_outputs());
                let run_before = before.as_ref().and_then(|_| self.run_print_if_stamped());
                self.take_rerun();
                let applied = match refused {
                    Some(e) => Err(e),
                    None => Update::decode_v1(&bytes).map_err(|e| e.to_string()),
                }
                .and_then(|u| {
                    let mut txn = self.doc().transact_mut();
                    txn.apply_update(u).map_err(|e| e.to_string())?;
                    // The checks above refuse what yrs would hold for later.
                    // Should it hold something all the same, drop it: it
                    // would be integrated by a later update, unchecked.
                    if let Some(p) = yrs::WriteTxn::store_mut(&mut txn).pending_update_mut() {
                        if !p.update.is_empty() {
                            tracing::warn!("dropping a held update the depth check didn't foresee");
                            p.update = Update::new();
                        }
                    }
                    Ok(())
                });
                match depths {
                    Some(d) if applied.is_ok() && !d.estimated => {
                        for (client, clock, depth) in d.types {
                            self.depths.insert((client, clock), depth);
                        }
                        self.shared_depth = self.shared_depth.max(d.deepest);
                    }
                    // A depth stood in for one unknown: measure for real.
                    Some(_) if applied.is_ok() => self.measure_depths(),
                    _ => {}
                }
                if let Err(e) = applied {
                    // jupyter-collaboration tells the client, which offers
                    // to reload.
                    tracing::warn!("update from client {conn} not applied: {e}");
                    fx.send
                        .push((Target::One(conn), raw_message(&json!({"type": "conflict"}))));
                }
                if self.read_only && self.content_changed.swap(false, Ordering::Relaxed) {
                    // Put the content back as it was loaded, and send that
                    // to everyone: the edit and its undoing to the others,
                    // the undoing to its sender.
                    tracing::debug!("read-only room: edit from client {conn} refused");
                    self.restore_baseline();
                    for u in self.drain() {
                        fx.send.push((
                            Target::All,
                            Message::Sync(SyncMessage::Update(u)).encode_v1(),
                        ));
                    }
                    return Ok(fx);
                }
                let updates = self.drain();
                if self.content_changed.swap(false, Ordering::Relaxed) {
                    self.generation += 1;
                    fx.changed = true;
                    if let Some(before) = before {
                        let author = Author {
                            name: self.client_name(conn),
                            person: true,
                        };
                        self.attribute(&before, &author);
                        let had = had_outputs.unwrap_or_default();
                        let rerun: HashSet<String> = self
                            .take_rerun()
                            .into_iter()
                            .filter(|(id, count)| *count || had.contains(id))
                            .map(|(id, _)| id)
                            .collect();
                        self.clear_run_notes(&before, &rerun);
                    }
                }
                if !updates.is_empty() {
                    for u in updates {
                        fx.send.push((
                            Target::Others(conn),
                            Message::Sync(SyncMessage::Update(u)).encode_v1(),
                        ));
                    }
                }
                fx.merge(self.broadcast_own());
                if let Some(run) = run_before {
                    fx.merge(self.clear_stale_clean_run(run));
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

    /// Set the content back to [`Room::baseline`], the file as last read.
    fn restore_baseline(&mut self) {
        let (Some(roots), Some(baseline)) = (self.roots.clone(), self.baseline.clone()) else {
            return;
        };
        if let Ok(notebook) = Notebook::from_value(baseline) {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set(&mut txn, &roots, &notebook);
        }
        self.content_changed.store(false, Ordering::Relaxed);
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

    /// [`Room::notebook`] with every cell's id, even in a notebook whose
    /// format (4.4 and older) keeps none on disk.
    pub fn notebook_with_ids(&self) -> Option<Result<Notebook, SchemaError>> {
        let roots = self.roots.as_ref()?;
        Some(schema::get_with_ids(
            &self.doc().transact(),
            roots,
            self.baseline.as_ref(),
        ))
    }

    /// The outputs of code cell `id`, as [`Room::notebook`] would write
    /// them: numbers Yjs can't hold come back from the file as last read.
    /// `None` when there is no such cell or the room is transient.
    pub fn cell_outputs(&self, id: &str) -> Option<Value> {
        use yrs::{Map, Transact};
        let roots = self.roots.as_ref()?;
        let txn = self.doc().transact();
        let outputs = schema::find_cell(&txn, roots, id)?.get(&txn, "outputs")?;
        let outputs = schema::out_to_json(&txn, &outputs);
        // The same cell in the file: by id, or by place in a notebook
        // that keeps no ids. A wrong match restores nothing.
        let old = self.baseline.as_ref().and_then(|b| {
            let cells = b["cells"].as_array()?;
            cells.iter().find(|c| c["id"] == id).or_else(|| {
                let i = schema::cell_index(&txn, roots, id)?;
                cells.get(i as usize).filter(|c| c.get("id").is_none())
            })
        });
        let old = old.map(|c| &c["outputs"]);
        Some(match old {
            Some(old) => schema::restore_numbers(outputs, old),
            None => outputs,
        })
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

    /// Tell clients whether the room's edits reached the file: `state.save`
    /// is `"saved"` or `"failed"`, and `state.save_error` holds
    /// the last error while it is `"failed"` and is removed otherwise.
    /// Room-only: the save never writes it. Nothing is sent when it is
    /// already so.
    pub fn set_save_state(&mut self, save: &str, error: Option<&str>) -> Effects {
        let Some(roots) = self.roots.clone() else {
            return Effects::default();
        };
        if self.state_value("save") == Some(json!(save))
            && self.state_value("save_error") == error.map(|e| json!(e))
        {
            return Effects::default();
        }
        {
            let mut txn = self.awareness.doc().transact_mut();
            roots.state.insert(&mut txn, "save", save);
            match error {
                Some(e) => {
                    roots.state.insert(&mut txn, "save_error", e);
                }
                None => {
                    roots.state.remove(&mut txn, "save_error");
                }
            }
        }
        self.broadcast_own()
    }

    /// Set `state.recovery`, the summary of the newest recovery copy of
    /// this notebook (`{"id", "at", "reason", "cells"}`), or remove it.
    /// Room-only, like [`Room::set_save_state`].
    pub fn set_recovery(&mut self, summary: Option<&Value>) -> Effects {
        let Some(roots) = self.roots.clone() else {
            return Effects::default();
        };
        if self.state_value("recovery").as_ref() == summary {
            return Effects::default();
        }
        {
            let mut txn = self.awareness.doc().transact_mut();
            match summary {
                Some(v) => {
                    roots
                        .state
                        .insert(&mut txn, "recovery", schema::json_to_any(v));
                }
                None => {
                    roots.state.remove(&mut txn, "recovery");
                }
            }
        }
        self.broadcast_own()
    }

    /// The value under `key` in the room's `state` map, if it is a plain
    /// value.
    pub fn state_value(&self, key: &str) -> Option<Value> {
        let roots = self.roots.as_ref()?;
        let txn = self.awareness.doc().transact();
        match roots.state.get(&txn, key)? {
            yrs::Out::Any(any) => Some(schema::any_to_json(&any)),
            _ => None,
        }
    }

    /// The room's `clean_run` stamp, if the notebook has one.
    fn clean_run(&self) -> Option<Value> {
        let roots = self.roots.as_ref()?;
        schema::clean_run(&self.doc().transact(), roots)
    }

    /// After the file's content went into the room: its cells changed in a
    /// way that makes a run stale, and the file kept the stamp the room had
    /// (so whoever wrote it didn't stamp it): remove the stamp, as a server
    /// edit that leaves the room unsaved. A file with another stamp, or
    /// none, is as its writer meant it, and so is any file in a read-only
    /// room, whose owner decides.
    fn clear_clean_run_for_file(&mut self, run: Option<u64>, stamp: Option<Value>) -> Effects {
        match run {
            Some(run) if !self.read_only && self.clean_run() == stamp => {
                self.clear_stale_clean_run(run)
            }
            _ => Effects::default(),
        }
    }

    /// Remove the room's `clean_run` stamp when its fingerprint is missing
    /// or doesn't match the cells ([`schema::clean_run_status`] is
    /// `Stale`). A stamp of a fingerprint version this code doesn't know,
    /// and any stamp in a read-only room (its owner decides), stay.
    /// Returns whether the stamp went. Doesn't touch the generation or
    /// send anything.
    fn remove_unfit_clean_run(&mut self) -> bool {
        if self.read_only {
            return false;
        }
        let Some(Ok(nb)) = self.notebook() else {
            return false;
        };
        if schema::clean_run_status(&nb) != schema::CleanRun::Stale {
            return false;
        }
        let Some(roots) = self.roots.clone() else {
            return false;
        };
        {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set_clean_run(&mut txn, &roots, None);
        }
        self.content_changed.store(false, Ordering::Relaxed);
        true
    }

    /// After the file's content went into the room by `reload` or
    /// `merge_disk`: remove a stamp that doesn't fit the cells (see
    /// `remove_unfit_clean_run`) as a server edit that leaves the room
    /// unsaved, so the file is rewritten without it.
    fn clear_unfit_clean_run(&mut self) -> Effects {
        if !self.remove_unfit_clean_run() {
            return Effects::default();
        }
        self.generation += 1;
        let mut fx = self.broadcast_own();
        fx.changed = true;
        fx
    }

    /// A host calls this when a room has opened a file, and when a
    /// read-only room becomes writable: remove a stamp that doesn't fit the
    /// cells from the room only. The room stays saved (no autosave, and
    /// `state.dirty` stays false) and the file keeps the stamp until the
    /// next save, which doesn't write it, so opening a file never writes
    /// it. Does nothing in a read-only room. A room with unsaved edits stays
    /// unsaved.
    pub fn forget_unfit_clean_run(&mut self) -> Effects {
        let clean = !self.is_dirty();
        if !self.remove_unfit_clean_run() {
            return Effects::default();
        }
        self.generation += 1;
        if clean {
            self.saved_generation = self.generation;
            if let Some(g) = self
                .baseline
                .as_mut()
                .and_then(|b| b.pointer_mut("/metadata/ganymede"))
                .and_then(Value::as_object_mut)
            {
                g.remove("clean_run");
            }
        }
        self.broadcast_own()
    }

    /// The file changed on disk: make the document match it, granularly
    /// (see [`schema::set`]), and mark it clean at `hash`. A change to the
    /// cells that makes a run stale removes the notebook's `clean_run`
    /// stamp, unless the file has a stamp of its own; the room is unsaved
    /// then, so the file is rewritten without it.
    pub fn reload(&mut self, notebook: &Notebook, hash: &str) -> Effects {
        let Some(roots) = self.roots.clone() else {
            return Effects::default();
        };
        let before = self.prints();
        let (run, stamp) = (self.run_print_if_stamped(), self.clean_run());
        {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set(&mut txn, &roots, notebook);
        }
        self.content_changed.store(false, Ordering::Relaxed);
        self.attribute(&before, &from_file());
        self.clear_run_notes(&before, &HashSet::new());
        self.generation += 1;
        self.saved_generation = self.generation;
        self.baseline = Some(schema::notebook_value(notebook));
        let mut fx = self.broadcast_own();
        // Before the state goes out, so it says the room is unsaved.
        fx.merge(self.clear_clean_run_for_file(run, stamp));
        fx.merge(self.clear_unfit_clean_run());
        fx.merge(self.set_state(hash));
        fx
    }

    /// The file changed on disk while the room may hold unsaved edits.
    /// `base` is the file as the host last read or wrote it, `disk` the
    /// file now, at `hash`.
    ///
    /// A clean room just [reloads](Room::reload). A dirty one merges (see
    /// [`crate::merge`]): the disk's changes are applied on top of the
    /// room's edits, which stay unsaved, so the host saves the merge next.
    /// Returns the parts where the room's edits lost to the disk.
    pub fn merge_disk(
        &mut self,
        base: &Notebook,
        disk: &Notebook,
        hash: &str,
    ) -> (Effects, Vec<String>) {
        if !self.is_dirty() {
            return (self.reload(disk, hash), Vec::new());
        }
        let ours = match self.notebook() {
            Some(Ok(nb)) => schema::notebook_value(&nb),
            Some(Err(_)) => return (self.reload(disk, hash), vec!["all".into()]),
            None => return (Effects::default(), Vec::new()),
        };
        let theirs = schema::notebook_value(disk);
        let merged = crate::merge::merge(&schema::notebook_value(base), &ours, &theirs);
        if merged.notebook == theirs {
            return (self.reload(disk, hash), merged.conflicts);
        }
        let Ok(notebook) = Notebook::from_value(merged.notebook) else {
            return (self.reload(disk, hash), vec!["all".into()]);
        };
        let Some(roots) = self.roots.clone() else {
            return (Effects::default(), Vec::new());
        };
        let before = self.prints();
        let (run, stamp) = (self.run_print_if_stamped(), self.clean_run());
        {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set(&mut txn, &roots, &notebook);
        }
        self.content_changed.store(false, Ordering::Relaxed);
        self.attribute(&before, &from_file());
        self.clear_run_notes(&before, &HashSet::new());
        self.generation += 1;
        self.baseline = Some(theirs);
        let mut fx = self.broadcast_own();
        fx.merge(self.clear_clean_run_for_file(run, stamp));
        fx.merge(self.clear_unfit_clean_run());
        fx.merge(self.set_state(hash));
        fx.changed = true;
        (fx, merged.conflicts)
    }

    /// Who last changed cell `id`'s source or type while the room was
    /// open, if anyone.
    pub fn author(&self, id: &str) -> Option<&Author> {
        self.authors.get(id)
    }

    /// Record `author` for cells `ids`: for an edit made with
    /// [`Room::edit`] on someone's behalf, such as an agent's.
    /// Ids the room doesn't hold (a deleted cell) are left out.
    pub fn set_author(&mut self, ids: &[String], author: &Author) {
        let Some(roots) = self.roots.clone() else {
            return;
        };
        let held: Vec<String> = {
            let txn = self.doc().transact();
            ids.iter()
                .filter(|id| schema::cell_index(&txn, &roots, id).is_some())
                .cloned()
                .collect()
        };
        // The edit just made changed cells (a deleted one too, which
        // `held` leaves out): its change is this author's. An edit that
        // changed none leaves `last_change` as it was.
        if std::mem::take(&mut self.edit_unclaimed) {
            if let Some((_, by)) = self.last_change.as_mut() {
                *by = author.clone();
            }
        }
        for id in held {
            self.authors.insert(id, author.clone());
        }
        // A deleted cell's credit goes with it.
        for id in ids {
            if schema::cell_index(&self.doc().transact(), &roots, id).is_none() {
                self.authors.remove(id);
            }
        }
    }

    /// Each cell's id and a hash of its type and source.
    fn prints(&self) -> HashMap<String, u64> {
        use std::hash::Hasher;
        use yrs::{Array, Out};
        let Some(roots) = &self.roots else {
            return HashMap::new();
        };
        let txn = self.doc().transact();
        let mut out = HashMap::new();
        for cell in roots.cells.iter(&txn) {
            let Out::YMap(m) = cell else { continue };
            let Some(Out::Any(yrs::Any::String(id))) = m.get(&txn, "id") else {
                continue;
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hash_type_and_source(&txn, &m, &mut h);
            out.insert(id.to_string(), h.finish());
        }
        out
    }

    /// The ids of the cells whose outputs aren't empty.
    fn cells_with_outputs(&self) -> HashSet<String> {
        use yrs::{Array, Out};
        let Some(roots) = &self.roots else {
            return HashSet::new();
        };
        let txn = self.doc().transact();
        let mut out = HashSet::new();
        for cell in roots.cells.iter(&txn) {
            let Out::YMap(m) = cell else { continue };
            let Some(Out::Any(yrs::Any::String(id))) = m.get(&txn, "id") else {
                continue;
            };
            if !outputs_empty(&txn, &m) {
                out.insert(id.to_string());
            }
        }
        out
    }

    /// The cells in [`Room::rerun`], which is empty afterwards.
    fn take_rerun(&self) -> HashMap<String, bool> {
        std::mem::take(&mut *self.rerun.lock().expect("rerun cells"))
    }

    /// After a change to the document: remove what the cells say of a run
    /// that the change outdates. A cell in `rerun`, which a client ran (or
    /// cleared) itself, loses `ran_source_hash` and `execution_skipped`;
    /// one whose source or type is no longer `before`'s loses
    /// `execution_skipped`. A server edit, left for the caller to send and
    /// count as part of the change.
    fn clear_run_notes(&mut self, before: &HashMap<String, u64>, rerun: &HashSet<String>) {
        use yrs::{Array, Out};
        let Some(roots) = self.roots.clone() else {
            return;
        };
        let mut edits: Vec<(yrs::MapRef, bool)> = Vec::new();
        {
            use std::hash::Hasher;
            let txn = self.doc().transact();
            for cell in roots.cells.iter(&txn) {
                let Out::YMap(m) = cell else { continue };
                let Some(Out::Any(yrs::Any::String(id))) = m.get(&txn, "id") else {
                    continue;
                };
                let reran = rerun.contains(&*id);
                let edited = || {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    hash_type_and_source(&txn, &m, &mut h);
                    before.get(&*id).is_some_and(|fp| *fp != h.finish())
                };
                if reran || (m.contains_key(&txn, "execution_skipped") && edited()) {
                    edits.push((m, reran));
                }
            }
        }
        if edits.is_empty() {
            return;
        }
        {
            let mut txn = self.awareness.doc().transact_mut();
            for (cell, reran) in edits {
                cell.remove(&mut txn, "execution_skipped");
                if reran {
                    schema::clear_ran_source_hash(&mut txn, &cell);
                }
            }
        }
        self.content_changed.store(false, Ordering::Relaxed);
    }

    /// A hash of what a full run of the notebook depends on and leaves
    /// behind: the cells in order, each with its type and source, and for
    /// a code cell its `execution_count`. Outputs, `execution_state`,
    /// cell metadata and notebook metadata are left out: they change as
    /// cells run or are annotated, without making a run stale.
    fn run_print(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        use yrs::{Array, Out};
        let Some(roots) = &self.roots else {
            return 0;
        };
        let txn = self.doc().transact();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for cell in roots.cells.iter(&txn) {
            let Out::YMap(m) = cell else {
                // Not a cell: mark the place all the same.
                1u8.hash(&mut h);
                continue;
            };
            match m.get(&txn, "id") {
                Some(Out::Any(yrs::Any::String(id))) => id.hash(&mut h),
                _ => "".hash(&mut h),
            }
            let code = hash_type_and_source(&txn, &m, &mut h);
            if code {
                match m.get(&txn, "execution_count") {
                    Some(o) => schema::out_to_json(&txn, &o).to_string().hash(&mut h),
                    None => "".hash(&mut h),
                }
            }
        }
        h.finish()
    }

    /// [`Room::run_print`] when the notebook carries a `clean_run` stamp,
    /// the only time a change to it matters.
    fn run_print_if_stamped(&self) -> Option<u64> {
        let roots = self.roots.as_ref()?;
        schema::clean_run(&self.doc().transact(), roots)?;
        Some(self.run_print())
    }

    /// After a change to the document: remove the `clean_run` stamp when
    /// [`Room::run_print`] is no longer `before`, a run's fingerprint taken
    /// before the change. The removal is a server edit, sent to every
    /// client; it changes only metadata, so it doesn't change the print
    /// itself.
    fn clear_stale_clean_run(&mut self, before: u64) -> Effects {
        let Some(roots) = self.roots.clone() else {
            return Effects::default();
        };
        if self.run_print() == before {
            return Effects::default();
        }
        {
            let mut txn = self.awareness.doc().transact_mut();
            schema::set_clean_run(&mut txn, &roots, None);
        }
        self.content_changed.store(false, Ordering::Relaxed);
        self.generation += 1;
        let mut fx = self.broadcast_own();
        fx.changed = true;
        fx
    }

    /// Credit `author` with each cell whose type or source differs from
    /// `before`, or that `before` lacks; forget cells that went.
    /// A cell that comes back with the hash it had when it went (an undo
    /// of its deletion) gets its old author back, not `author`. Returns
    /// whether a cell was added, removed or changed.
    fn attribute(&mut self, before: &HashMap<String, u64>, author: &Author) -> bool {
        let now = self.prints();
        let mut changed = before.keys().any(|id| !now.contains_key(id));
        for (id, fp) in &now {
            if before.get(id) == Some(fp) {
                continue;
            }
            changed = true;
            // Only a client's update restores: a cell coming back from the
            // file, or from the server, is theirs.
            let back = (!before.contains_key(id))
                .then(|| self.gone.remove(id))
                .flatten()
                .filter(|(was, _)| was == fp && author.person)
                .map(|(_, a)| a);
            match back {
                Some(a) => self.authors.insert(id.clone(), a),
                None => self.authors.insert(id.clone(), author.clone()),
            };
        }
        // Every cell that went, authored or not: one nobody changed since
        // the room opened came from the file, and comes back as the file's.
        for (id, fp) in before {
            if now.contains_key(id) {
                continue;
            }
            let a = self.authors.remove(id).unwrap_or_else(from_file);
            if self.gone.len() >= GONE_LIMIT {
                if let Some(k) = self.gone.keys().next().cloned() {
                    self.gone.remove(&k);
                }
            }
            self.gone.insert(id.clone(), (*fp, a));
        }
        self.authors.retain(|id, _| now.contains_key(id));
        if changed {
            self.last_change = Some((std::time::SystemTime::now(), author.clone()));
        }
        changed
    }

    /// The name the server vouches for on connection `conn`, else the one
    /// its client gave in its awareness state.
    fn client_name(&self, conn: ConnId) -> String {
        if let Some(name) = self.vouched.get(&conn) {
            return name.clone();
        }
        let named = |v: &Value| {
            let user = v.get("user")?;
            ["display_name", "name", "username"]
                .iter()
                .filter_map(|k| user.get(*k)?.as_str())
                .map(clean_name)
                .find(|s| !s.is_empty())
        };
        self.conns
            .get(&conn)
            .into_iter()
            .flatten()
            .filter_map(|id| self.awareness.state::<Value>(*id))
            .find_map(|v| named(&v))
            .unwrap_or_else(|| UNNAMED.to_string())
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
        self.edit_unclaimed = false;
        let before = self.prints();
        let run_before = self.run_print_if_stamped();
        let r = {
            let mut txn = self.awareness.doc().transact_mut();
            f(&mut txn, &roots)
        };
        let mut fx = self.broadcast_own();
        if self.content_changed.swap(false, Ordering::Relaxed) && !self.read_only {
            self.generation += 1;
            fx.changed = true;
            // A cell the server changed is no longer a person's, whoever
            // edits through it (a kernel through the host API, an agent):
            // the caller may name itself with `set_author`.
            let server = Author {
                name: SERVER.to_string(),
                person: false,
            };
            self.edit_unclaimed = self.attribute(&before, &server);
            self.clear_run_notes(&before, &HashSet::new());
            fx.merge(self.broadcast_own());
            if let Some(run) = run_before {
                fx.merge(self.clear_stale_clean_run(run));
            }
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
