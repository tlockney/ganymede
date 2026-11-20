//! Server-side execution into a document room: `jupyter-server-documents`'
//! `POST /api/kernels/{kernel_id}/execute` (#244).
//!
//! ```text
//! {"document_id": "json:notebook:<file id>",
//!  "cells": [{"cell_id": "...", "source_hash": "<MurmurHash2, seed 0>"}],
//!  "client_id": "...", "request_id": "...", "previous_request_id": "..."}
//! ```
//!
//! Ganymede's extensions (#566, #843): `wait`, and cells chosen against
//! the room's current source. An entry without `source_hash` (or with
//! `null`) runs whatever the room holds, unsaved edits included;
//! `{"tag": "x"}` names every code cell tagged `x`; `"cells": "all"` is
//! every code cell with source. The room resolves them in its task, in the
//! same step as the checks, so a notebook with no cell ids on disk
//! (nbformat 4.4) runs by the ids its room gave. `"by": "<name>"` (#896)
//! says who runs them: the caller's tailnet login if there is one, else
//! `by`, else "an API client", each cleaned like a room client's name.
//!
//! The room checks every given hash against its cell's current source,
//! then clears the cells' outputs, marks them running (`execution_state`,
//! and `execution_by` with the runner's name, both room-only: never
//! saved), and queues the batch, all in one step of its task, so no other
//! request interleaves with it. The answer is `200 null` once queued;
//! `409 {"error": "source_mismatch", "cell_id"}` when a cell's source
//! changed since the client hashed it; `408` when `previous_request_id` was not queued within
//! ten seconds; `400` for the rest, `"error": "cell_not_found"` among them
//! for an id or tag the room doesn't have. A room that isn't open is
//! opened, so an agent with no websocket can run cells.
//!
//! One worker per room runs the queue in order on the named kernel,
//! through `ganymede-exec`'s [`NotebookRun`] (display updates, clears,
//! counts), and writes outputs into the room as they arrive, so they land
//! with no client connected and are saved with the notebook. A cell goes
//! back to idle, with no `execution_by`, when its last queued run ends
//! (a cell queued behind another run of itself shows the run in progress). A
//! cell that fails stops the rest of its batch, as a kernel's
//! `stop_on_error` does for JupyterLab's Run All. Input requests are
//! refused (`allow_stdin` off).

use crate::collab::{send_to_notebook_room, Cmd};
use crate::room_ops;
use crate::routes::{HttpError, LenientJson};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use ganymede_api::KernelManager;
use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::{Any, Array, Map, MapRef, Out};
use ganymede_documents::{Effects, Room};
use ganymede_exec::{CellStatus, Event, NotebookRun, Options};
use ganymede_nbformat::Cell;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

/// How long a request waits for its `previous_request_id` to be queued.
const PREDECESSOR_TIMEOUT: Duration = Duration::from_secs(10);
/// A worker forgets the cells it ran (and so their display ids) after this
/// many, so a long session doesn't grow without bound.
const MAX_REMEMBERED_CELLS: usize = 256;
/// Queued request ids kept for successors; the oldest go first.
const MAX_QUEUED_IDS: usize = 10_000;
/// The longest `wait` a request may ask for.
const MAX_WAIT: Duration = Duration::from_secs(3600);
/// Who a run is credited to when a REST request names no one.
pub(crate) const API_CLIENT: &str = "an API client";

#[derive(Deserialize, Default)]
pub struct ExecuteBody {
    document_id: Option<String>,
    cells: Option<Value>,
    #[allow(dead_code)]
    client_id: Option<String>,
    request_id: Option<String>,
    previous_request_id: Option<String>,
    /// Ganymede's extension (#566): wait up to this many seconds for the
    /// batch to run, and answer with its cells.
    wait: Option<f64>,
    /// Ganymede's extension (#896): who runs the cells, shown in the room
    /// as `execution_by` while they run. A caller the server vouches for
    /// (a tailnet login) is shown by that name instead.
    by: Option<String>,
}

/// The name shown as running a REST request's cells: the caller's tailnet
/// login, else the body's `by`, else `API_CLIENT`. Each is cleaned like
/// a room client's name; one with nothing left yields to the next.
pub(crate) fn runner_name(login: Option<&str>, by: Option<&str>) -> String {
    [login, by]
        .into_iter()
        .flatten()
        .map(ganymede_documents::clean_name)
        .find(|n| !n.is_empty())
        .unwrap_or_else(|| API_CLIENT.to_string())
}

/// Which cells a request names.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Selector {
    /// A cell by id, with the source hash to check (`None`: don't check).
    Id(String, Option<String>),
    /// Every code cell tagged so.
    Tag(String),
    /// Every code cell with source.
    All,
}

/// Why a batch was refused.
#[derive(Debug)]
pub(crate) enum Refused {
    SourceMismatch(String),
    /// An id or tag the room doesn't have; `message` says so.
    NotFound {
        cell_id: Option<String>,
        tag: Option<String>,
        message: String,
    },
    Bad(String),
}

/// What the room task is asked, by the endpoint and by its worker.
pub(crate) enum RoomCmd {
    /// Check and queue a batch.
    Enqueue {
        kernels: Arc<KernelManager>,
        kernel_id: String,
        cells: Vec<Selector>,
        /// Who runs them, shown in the room as `execution_by`.
        by: String,
        /// The ids the selectors resolved to, in the order they run.
        reply: oneshot::Sender<Result<Vec<String>, Refused>>,
        /// Told when the whole batch has run.
        finished: Option<oneshot::Sender<()>>,
    },
    /// The worker wants a cell as it is now, to run it.
    Get(String, oneshot::Sender<Option<Cell>>),
    /// New outputs for a cell. A display update to a cell that already
    /// finished carries what the cell must still show for it to apply: its
    /// output count and execution count, so an update never brings back
    /// outputs a client cleared or a later run replaced.
    Outputs {
        cell_id: String,
        outputs: Vec<Value>,
        only_if: Option<(u32, Option<i64>)>,
    },
    /// Outputs the running cell added since the last `Outputs` or
    /// `Append`: appended as a frontend shows them, a stream merged into
    /// the stream before it. Costs what it adds, not what the cell shows.
    Append {
        cell_id: String,
        outputs: Vec<Value>,
    },
    /// A cell finished (or was skipped): its final outputs, if it ran, and
    /// the kernel's execution count.
    Done {
        cell_id: String,
        outputs: Option<Vec<Value>>,
        execution_count: Option<i64>,
    },
}

impl RoomCmd {
    /// The cell whose outputs this replaces with a run's, if it does. Not
    /// an `Append`, which keeps what the cell holds, nor a display update,
    /// which may not apply.
    pub(crate) fn ran_cell(&self) -> Option<&str> {
        match self {
            RoomCmd::Outputs {
                cell_id,
                only_if: None,
                ..
            }
            | RoomCmd::Done {
                cell_id,
                outputs: Some(_),
                ..
            } => Some(cell_id),
            _ => None,
        }
    }

    /// The room could not be opened.
    pub(crate) fn unavailable(self, why: &str) {
        if let RoomCmd::Enqueue { reply, .. } = self {
            let _ = reply.send(Err(Refused::Bad(why.to_string())));
        }
    }
}

struct Batch {
    kernels: Arc<KernelManager>,
    kernel_id: String,
    cells: Vec<String>,
    finished: Option<oneshot::Sender<()>>,
}

/// A room's side of execution: its queue and how many cells are pending.
pub(crate) struct RoomExec {
    me: mpsc::UnboundedSender<Cmd>,
    worker: Option<mpsc::UnboundedSender<Batch>>,
    pending: usize,
    /// Per cell id, who runs each queued or running run of it, in order:
    /// the front is the run in progress. Entries are dropped when empty.
    queued: HashMap<String, VecDeque<String>>,
}

impl RoomExec {
    pub(crate) fn new(me: mpsc::UnboundedSender<Cmd>) -> Self {
        RoomExec {
            me,
            worker: None,
            pending: 0,
            queued: HashMap::new(),
        }
    }

    /// Check and queue `selectors` for `by`: [`enqueue`], then remember
    /// who runs each cell, in the same step of the room's task.
    fn queue(
        &mut self,
        room: &mut Room,
        selectors: &[Selector],
        by: &str,
    ) -> (Result<Vec<String>, Refused>, Effects) {
        let (checked, fx) = enqueue(room, selectors, by, &self.queued);
        if let Ok(ids) = &checked {
            for id in ids {
                self.queued
                    .entry(id.clone())
                    .or_default()
                    .push_back(by.to_string());
            }
        }
        (checked, fx)
    }

    /// A run of `cell_id` ended (or was skipped): its outputs, if it ran.
    /// The cell stays running, now by whoever queued next, while it has
    /// runs queued; otherwise it is idle and has no `execution_by`.
    fn finish(
        &mut self,
        room: &mut Room,
        cell_id: &str,
        outputs: Option<Vec<Value>>,
        execution_count: Option<i64>,
    ) -> Effects {
        self.pending = self.pending.saturating_sub(1);
        // Also when the cell is gone, so a later cell of that id starts
        // with no stale name.
        let next = match self.queued.get_mut(cell_id) {
            Some(runs) => {
                runs.pop_front();
                let next = runs.front().cloned();
                if next.is_none() {
                    self.queued.remove(cell_id);
                }
                next
            }
            None => None,
        };
        room.edit(|txn, roots| {
            let Some(cell) = schema::find_cell(txn, roots, cell_id) else {
                return;
            };
            if let Some(outputs) = outputs {
                schema::set_outputs(txn, &cell, outputs);
            }
            if let Some(n) = execution_count {
                cell.insert(txn, "execution_count", schema::json_to_any(&json!(n)));
            }
            match next {
                Some(by) => {
                    // A client's own run may have marked the cell idle meanwhile.
                    cell.insert(txn, "execution_state", "running");
                    cell.insert(txn, "execution_by", by);
                }
                None => {
                    cell.insert(txn, "execution_state", "idle");
                    cell.remove(txn, "execution_by");
                }
            }
        })
        .1
    }

    /// No cell queued or running, so the room may close.
    pub(crate) fn is_idle(&self) -> bool {
        self.pending == 0
    }

    pub(crate) fn handle(&mut self, room: &mut Room, cmd: RoomCmd) -> Effects {
        match cmd {
            RoomCmd::Enqueue {
                kernels,
                kernel_id,
                cells,
                by,
                reply,
                finished,
            } => {
                let (checked, fx) = self.queue(room, &cells, &by);
                if let Ok(ids) = &checked {
                    if ids.is_empty() {
                        if let Some(f) = finished {
                            let _ = f.send(());
                        }
                        let _ = reply.send(checked);
                        return fx;
                    }
                    self.pending += ids.len();
                    let worker = self.worker.get_or_insert_with(|| {
                        let (tx, rx) = mpsc::unbounded_channel();
                        tokio::spawn(run_worker(self.me.clone(), rx));
                        tx
                    });
                    let _ = worker.send(Batch {
                        kernels,
                        kernel_id,
                        cells: ids.clone(),
                        finished,
                    });
                }
                let _ = reply.send(checked);
                fx
            }
            RoomCmd::Get(id, reply) => {
                let cell = room
                    .notebook_with_ids()
                    .and_then(Result::ok)
                    .and_then(|nb| nb.cells.into_iter().find(|c| c.id() == Some(id.as_str())));
                let _ = reply.send(cell);
                Effects::default()
            }
            RoomCmd::Outputs {
                cell_id,
                outputs,
                only_if,
            } => {
                room.edit(|txn, roots| {
                    let Some(cell) = schema::find_cell(txn, roots, &cell_id) else {
                        return;
                    };
                    if let Some((len, count)) = only_if {
                        let shown = match cell.get(txn, "outputs") {
                            Some(ganymede_documents::yrs::Out::YArray(a)) => {
                                ganymede_documents::yrs::Array::len(&a, txn)
                            }
                            _ => 0,
                        };
                        let shown_count = cell
                            .get(txn, "execution_count")
                            .map(|c| schema::out_to_json(txn, &c))
                            .and_then(|c| c.as_i64());
                        if shown != len || shown_count != count {
                            return;
                        }
                    }
                    schema::set_outputs(txn, &cell, outputs);
                })
                .1
            }
            RoomCmd::Append { cell_id, outputs } => {
                room.edit(|txn, roots| {
                    if let Some(cell) = schema::find_cell(txn, roots, &cell_id) {
                        schema::append_outputs(txn, &cell, outputs);
                    }
                })
                .1
            }
            RoomCmd::Done {
                cell_id,
                outputs,
                execution_count,
            } => self.finish(room, &cell_id, outputs, execution_count),
        }
    }
}

/// Resolve the selectors against the room's cells, check them, then clear
/// and mark every cell, or none. The ids come back in run order, each once.
///
/// A cell with no run queued gets `execution_by` = `by`; one already
/// queued (`queued`) keeps the name of the run in progress.
fn enqueue(
    room: &mut Room,
    selectors: &[Selector],
    by: &str,
    queued: &HashMap<String, VecDeque<String>>,
) -> (Result<Vec<String>, Refused>, Effects) {
    if room.notebook().is_none() {
        return (
            Err(Refused::Bad("not a notebook room".into())),
            Effects::default(),
        );
    }
    // Another server owns the file (#272): this room never saves, so the
    // cells' outputs would be lost.
    if !room_ops::writable(room) {
        return (
            Err(Refused::Bad(format!(
                "{}, so it is read-only here; run it there",
                room_ops::READ_ONLY
            ))),
            Effects::default(),
        );
    }
    room.edit(|txn, roots: &Roots| {
        let is_code = |cell: &MapRef| {
            matches!(cell.get(txn, "cell_type"), Some(Out::Any(Any::String(t))) if &*t == "code")
        };
        let id_of = |cell: &MapRef| match cell.get(txn, "id") {
            Some(Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        };
        let code_cells: Vec<(String, MapRef)> = roots
            .cells
            .iter(txn)
            .filter_map(|out| match out {
                Out::YMap(m) if is_code(&m) => id_of(&m).map(|id| (id, m)),
                _ => None,
            })
            .collect();
        let mut found: Vec<(String, MapRef)> = Vec::new();
        // A tag or "all" skips a cell already queued; an id given twice
        // runs twice, as in jupyter-server-documents.
        let mut add = |id: &str, cell: &MapRef, again: bool| {
            if again || !found.iter().any(|(f, _)| f == id) {
                found.push((id.to_string(), cell.clone()));
            }
        };
        for selector in selectors {
            match selector {
                Selector::Id(id, hash) => {
                    let Some(cell) = schema::find_cell(txn, roots, id) else {
                        let hint = if schema::keeps_no_ids(txn, roots) {
                            "; this notebook (nbformat 4.4) keeps no cell ids on disk, so its ids are the room's and last only while its room is open"
                        } else {
                            ""
                        };
                        return Err(Refused::NotFound {
                            cell_id: Some(id.clone()),
                            tag: None,
                            message: format!("Cell {id:?} not found in document{hint}"),
                        });
                    };
                    if !is_code(&cell) {
                        return Err(Refused::Bad(format!("Cell {id:?} is not a code cell")));
                    }
                    let stale = hash.as_ref().is_some_and(|h| {
                        let source = room_ops::source_of(txn, &cell).unwrap_or_default();
                        ganymede_documents::murmur2(source.as_bytes(), 0).to_string() != *h
                    });
                    if stale {
                        return Err(Refused::SourceMismatch(id.clone()));
                    }
                    add(id, &cell, true);
                }
                Selector::Tag(tag) => {
                    let mut any = false;
                    for (id, cell) in &code_cells {
                        let tagged = cell
                            .get(txn, "metadata")
                            .map(|m| schema::out_to_json(txn, &m))
                            .is_some_and(|m| {
                                m["tags"]
                                    .as_array()
                                    .is_some_and(|t| t.iter().any(|t| t.as_str() == Some(tag)))
                            });
                        if tagged {
                            any = true;
                            add(id, cell, false);
                        }
                    }
                    if !any {
                        return Err(Refused::NotFound {
                            cell_id: None,
                            tag: Some(tag.clone()),
                            message: format!("No code cell has the tag {tag:?} in document"),
                        });
                    }
                }
                Selector::All => {
                    for (id, cell) in &code_cells {
                        let source = room_ops::source_of(txn, cell).unwrap_or_default();
                        if !source.trim().is_empty() {
                            add(id, cell, false);
                        }
                    }
                }
            }
        }
        for (id, cell) in &found {
            schema::set_outputs(txn, cell, Vec::new());
            cell.insert(txn, "execution_state", "running");
            if queued.get(id).is_none_or(VecDeque::is_empty) {
                cell.insert(txn, "execution_by", by);
            }
        }
        Ok(found.into_iter().map(|(id, _)| id).collect())
    })
}

/// Runs a room's batches in order.
async fn run_worker(room: mpsc::UnboundedSender<Cmd>, mut rx: mpsc::UnboundedReceiver<Batch>) {
    let mut runs: HashMap<String, Run> = HashMap::new();
    while let Some(batch) = rx.recv().await {
        let mut stopped = false;
        for cell_id in batch.cells {
            if stopped {
                done(&room, cell_id, None, None);
                continue;
            }
            let run = runs.entry(batch.kernel_id.clone()).or_insert_with(Run::new);
            stopped = !run
                .cell(&room, &batch.kernels, &batch.kernel_id, cell_id)
                .await;
        }
        if let Some(f) = batch.finished {
            let _ = f.send(());
        }
    }
}

fn done(
    room: &mpsc::UnboundedSender<Cmd>,
    cell_id: String,
    outputs: Option<Vec<Value>>,
    execution_count: Option<i64>,
) {
    let _ = room.send(Cmd::Exec(RoomCmd::Done {
        cell_id,
        outputs,
        execution_count,
    }));
}

/// Cells run on one kernel, so display updates reach earlier cells.
struct Run {
    run: NotebookRun,
    cells: Vec<Cell>,
    /// The cell id of each run; empty once the cell has run again, so
    /// display updates no longer reach that copy.
    ids: Vec<String>,
    /// Each run's execution count.
    counts: Vec<Option<i64>>,
}

impl Run {
    fn new() -> Self {
        Run {
            run: NotebookRun::new(Options {
                record_timing: false,
                store_widget_state: false,
                coalesce_streams: true,
                ..Default::default()
            }),
            cells: Vec::new(),
            ids: Vec::new(),
            counts: Vec::new(),
        }
    }

    /// Run one cell; false when it failed and the batch should stop.
    async fn cell(
        &mut self,
        room: &mpsc::UnboundedSender<Cmd>,
        kernels: &KernelManager,
        kernel_id: &str,
        cell_id: String,
    ) -> bool {
        let kernel = match kernels.wait_started(kernel_id).await {
            Ok(_) => kernels.get(kernel_id),
            Err(e) => Err(e),
        };
        let kernel = match kernel {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(kernel = %kernel_id, "cannot run cell {cell_id}: {}", e.message);
                done(room, cell_id, None, None);
                return false;
            }
        };
        let (tx, rx) = oneshot::channel();
        let _ = room.send(Cmd::Exec(RoomCmd::Get(cell_id.clone(), tx)));
        let Ok(Some(Cell::Code(mut cell))) = rx.await else {
            done(room, cell_id, None, None);
            return true; // deleted since it was queued
        };
        cell.outputs.clear();
        if self.cells.len() >= MAX_REMEMBERED_CELLS {
            *self = Run::new();
        }
        for id in self.ids.iter_mut().filter(|id| **id == cell_id) {
            id.clear();
        }
        self.cells.push(Cell::Code(cell));
        self.ids.push(cell_id.clone());
        self.counts.push(None);
        let index = self.cells.len() - 1;
        kernel.touch();

        let mut count: Option<i64> = None;
        let ids = self.ids.clone();
        let counts = self.counts.clone();
        let mut live = LiveOutputs::default();
        let mut handler = |event: Event<'_>| {
            if let Event::Output {
                msg,
                changed,
                cells,
                ..
            } = event
            {
                let msg_type = msg.msg_type();
                if msg_type == "execute_input" {
                    count = msg.content["execution_count"].as_i64();
                }
                let clear = msg_type == "clear_output";
                if clear {
                    live.cleared();
                }
                let display = msg.content["transient"]["display_id"].is_string();
                for &i in changed {
                    let (Some(Cell::Code(c)), Some(id)) = (cells.get(i), ids.get(i)) else {
                        continue;
                    };
                    if id.is_empty() {
                        continue;
                    }
                    if i == index {
                        if let Some(from) = live.next(clear, display, c.outputs.len()) {
                            let outputs = live_outputs(&c.outputs[from..]);
                            if !outputs.is_empty() {
                                let _ = room.send(Cmd::Exec(RoomCmd::Append {
                                    cell_id: id.clone(),
                                    outputs,
                                }));
                            }
                            continue;
                        }
                    }
                    let outputs = live_outputs(&c.outputs);
                    // An update to an earlier cell (`update_display`) changes
                    // an output in place, so the count is the one it shows.
                    let only_if = (i != index)
                        .then(|| (outputs.len() as u32, counts.get(i).copied().flatten()));
                    let _ = room.send(Cmd::Exec(RoomCmd::Outputs {
                        cell_id: id.clone(),
                        outputs,
                        only_if,
                    }));
                }
            }
        };
        let status = self
            .run
            .execute_cell_with(&kernel.session(), &mut self.cells, index, &mut handler)
            .await;
        kernel.touch();
        self.counts[index] = count;
        let outputs = match &self.cells[index] {
            Cell::Code(c) => Some(outputs_json(&c.outputs)),
            _ => None,
        };
        let ok = match status {
            Ok(CellStatus::Ok) => true,
            Ok(CellStatus::Error { .. }) => false,
            Err(e) => {
                tracing::warn!(kernel = %kernel_id, "cell {cell_id}: {e}");
                false
            }
        };
        done(room, cell_id, outputs, count);
        ok
    }
}

/// Which of the running cell's outputs the room already has. Outputs that
/// only add to them go as an `Append` of the new ones; a message that may
/// change what is there (a clear, a display with an id) resends them all.
struct LiveOutputs {
    sent: usize,
    /// Resend everything at the next update. Set at the start, since the
    /// room may show outputs from before this run (the same cell queued
    /// twice, a client's edit), and by a clear: a clear with `wait`
    /// changes nothing until the next output, so the clear and the output
    /// after it both resend.
    resync: bool,
}

impl Default for LiveOutputs {
    fn default() -> Self {
        LiveOutputs {
            sent: 0,
            resync: true,
        }
    }
}

impl LiveOutputs {
    /// A `clear_output` arrived for the running cell. Called for every
    /// clear, including a `wait` one that changes no output yet and so
    /// never reaches [`LiveOutputs::next`].
    fn cleared(&mut self) {
        self.resync = true;
    }

    /// The cell now has `len` outputs after a message that was a `clear`,
    /// or carried a `display` id. `Some(from)`: append the outputs from
    /// `from` on. `None`: resend them all.
    fn next(&mut self, clear: bool, display: bool, len: usize) -> Option<usize> {
        self.resync |= clear;
        let append = (!self.resync && !display && len >= self.sent).then_some(self.sent);
        if append.is_none() {
            self.resync = clear;
        }
        self.sent = len;
        append
    }
}

fn outputs_json(outputs: &[ganymede_nbformat::Output]) -> Vec<Value> {
    outputs
        .iter()
        .map(|o| serde_json::to_value(o).expect("an output serializes"))
        .collect()
}

/// Outputs as a frontend shows them while the cell runs: consecutive
/// streams of the same name merged, as JupyterLab's output area does, so
/// a growing stream is one output and is sent as appended text.
fn live_outputs(outputs: &[ganymede_nbformat::Output]) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for o in outputs_json(outputs) {
        if let Some(last) = merged.last_mut() {
            let both_streams = last["output_type"] == "stream" && o["output_type"] == "stream";
            if both_streams && last["name"] == o["name"] {
                if let (Some(a), Some(b)) = (last["text"].as_str(), o["text"].as_str()) {
                    last["text"] = Value::String(format!("{a}{b}"));
                    continue;
                }
            }
        }
        merged.push(o);
    }
    merged
}

/// Which request ids have been queued, for `previous_request_id`.
#[derive(Default)]
pub struct Requests {
    queued: Mutex<HashMap<String, (watch::Sender<bool>, Instant)>>,
}

impl Requests {
    /// Forgets ids that were waited for but never queued, after a minute.
    /// A queued id stays until a successor has waited on it, as
    /// jupyter-server-documents keeps it; at most [`MAX_QUEUED_IDS`].
    fn prune(q: &mut HashMap<String, (watch::Sender<bool>, Instant)>) {
        q.retain(|_, (tx, t)| *tx.borrow() || t.elapsed() < Duration::from_secs(60));
        if q.len() > MAX_QUEUED_IDS {
            let mut by_age: Vec<(Instant, String)> =
                q.iter().map(|(id, (_, t))| (*t, id.clone())).collect();
            by_age.sort();
            for (_, id) in by_age.into_iter().take(q.len() - MAX_QUEUED_IDS) {
                q.remove(&id);
            }
        }
    }

    fn channel(&self, id: &str) -> watch::Receiver<bool> {
        let mut q = self.queued.lock().expect("requests");
        Self::prune(&mut q);
        q.entry(id.to_string())
            .or_insert_with(|| (watch::channel(false).0, Instant::now()))
            .0
            .subscribe()
    }

    /// A successor waited on `id` and found it queued.
    fn consumed(&self, id: &str) {
        let mut q = self.queued.lock().expect("requests");
        if q.get(id).is_some_and(|(tx, _)| *tx.borrow()) {
            q.remove(id);
        }
    }

    fn mark(&self, id: &str) {
        let mut q = self.queued.lock().expect("requests");
        Self::prune(&mut q);
        let (tx, _) = q
            .entry(id.to_string())
            .or_insert_with(|| (watch::channel(false).0, Instant::now()));
        tx.send_replace(true);
    }
}

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError(400, msg.into())
}

/// The request's `cells`: `"all"`, or a non-empty list of `{cell_id,
/// source_hash?}` and `{tag}` entries.
fn parse_cells(cells: Option<Value>) -> Result<Vec<Selector>, HttpError> {
    let entries = match cells {
        Some(Value::String(s)) if s == "all" => return Ok(vec![Selector::All]),
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => {
            return Err(bad(
                "cells must be \"all\" or a non-empty list of {cell_id, source_hash} or {tag}",
            ))
        }
    };
    entries
        .iter()
        .map(|entry| {
            let wrong = || {
                bad(format!(
                    "Each cell entry must have a cell_id string or a tag string, not both, got {entry}"
                ))
            };
            let Value::Object(o) = entry else {
                return Err(wrong());
            };
            match (o.get("cell_id"), o.get("tag")) {
                (Some(Value::String(id)), None) => {
                    let hash = match o.get("source_hash") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(s)) => Some(s.clone()),
                        Some(Value::Number(n)) => Some(n.to_string()),
                        Some(_) => {
                            return Err(bad(format!(
                                "source_hash must be a string or number for cell {id:?}"
                            )))
                        }
                    };
                    Ok(Selector::Id(id.clone(), hash))
                }
                (None, Some(Value::String(tag))) => Ok(Selector::Tag(tag.clone())),
                _ => Err(wrong()),
            }
        })
        .collect()
}

pub async fn execute(
    State(state): State<AppState>,
    Path(kernel_id): Path<String>,
    user: Option<axum::Extension<crate::auth::TailnetUser>>,
    LenientJson(body): LenientJson<ExecuteBody>,
) -> Result<Response, HttpError> {
    let by = runner_name(
        user.as_ref().map(|axum::Extension(u)| u.0.as_str()),
        body.by.as_deref(),
    );
    let document_id = body
        .document_id
        .filter(|d| !d.is_empty())
        .ok_or_else(|| bad("document_id is required"))?;
    let cells = parse_cells(body.cells)?;
    let wait = match body.wait {
        None => None,
        // Clamped first: `from_secs_f64` panics past `Duration::MAX`.
        Some(w) if w.is_finite() && w > 0.0 => {
            Some(Duration::from_secs_f64(w.min(MAX_WAIT.as_secs_f64())))
        }
        Some(w) => {
            return Err(bad(format!(
                "wait must be a positive number of seconds, got {w}"
            )))
        }
    };
    // 404 for an unknown kernel, as the other kernel endpoints answer.
    state.kernels.get(&kernel_id).map_err(HttpError::from)?;

    if let Some(prev) = body
        .previous_request_id
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        let mut rx = state.collab.requests.channel(prev);
        let waited = tokio::time::timeout(PREDECESSOR_TIMEOUT, rx.wait_for(|q| *q))
            .await
            .is_ok_and(|r| r.is_ok());
        if !waited {
            return Err(HttpError(
                408,
                "Timed out waiting for previous_request_id to be enqueued".into(),
            ));
        }
        state.collab.requests.consumed(prev);
    }

    let (reply, answer) = oneshot::channel();
    let (finished, done) = match wait {
        Some(_) => {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    let cmd = Cmd::Exec(RoomCmd::Enqueue {
        kernels: state.kernels.clone(),
        kernel_id,
        cells,
        by,
        reply,
        finished,
    });
    let room = send_to_notebook_room(&state.collab, &state.contents, &document_id, cmd)
        .await
        .ok_or_else(|| bad(format!("No YRoom available for document: {document_id:?}")))?;
    let ids = match answer.await {
        Ok(Ok(ids)) => ids,
        Ok(Err(Refused::NotFound {
            cell_id,
            tag,
            message,
        })) => {
            let mut body = json!({"message": message, "reason": null, "error": "cell_not_found"});
            if let Some(id) = cell_id {
                body["cell_id"] = json!(id);
            }
            if let Some(tag) = tag {
                body["tag"] = json!(tag);
            }
            return Ok((StatusCode::BAD_REQUEST, axum::Json(body)).into_response());
        }
        Ok(Err(Refused::SourceMismatch(cell_id))) => {
            return Ok((
                StatusCode::CONFLICT,
                axum::Json(json!({"error": "source_mismatch", "cell_id": cell_id})),
            )
                .into_response())
        }
        Ok(Err(Refused::Bad(msg))) => return Err(bad(msg)),
        Err(_) => return Err(bad(format!("Room {document_id:?} closed"))),
    };
    if let Some(id) = body.request_id.as_deref().filter(|p| !p.is_empty()) {
        state.collab.requests.mark(id);
    }
    if let (Some(wait), Some(done)) = (wait, done) {
        let finished = tokio::time::timeout(wait, done)
            .await
            .is_ok_and(|r| r.is_ok());
        let cells = room_cells(&room, ids).await;
        return Ok(axum::Json(json!({"finished": finished, "cells": cells})).into_response());
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "null",
    )
        .into_response())
}

/// The cells `ids` as the room has them now, in that order, as nbformat
/// JSON; a cell deleted since is left out. The room handles commands in
/// order, so asked after a batch finished, these carry its outputs.
async fn room_cells(room: &mpsc::UnboundedSender<Cmd>, ids: Vec<String>) -> Vec<Value> {
    let mut cells = Vec::new();
    for id in ids {
        let (tx, rx) = oneshot::channel();
        if room.send(Cmd::Exec(RoomCmd::Get(id, tx))).is_err() {
            break;
        }
        if let Ok(Some(cell)) = rx.await {
            cells.extend(serde_json::to_value(&cell).ok());
        }
    }
    cells
}

/// Queue cells of the notebook at `path` on `kernel_id`, unchecked, and
/// wait until they have run or `timeout` passes. False on a timeout; then
/// `late` runs once they have all run (or the room stops), in a task of
/// its own.
pub(crate) async fn run_and_wait(
    state: &AppState,
    path: &str,
    kernel_id: &str,
    cells: Vec<String>,
    by: String,
    timeout: Duration,
    late: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<bool, String> {
    let (reply, answer) = oneshot::channel();
    let (finished, done) = oneshot::channel();
    let cmd = Cmd::Exec(RoomCmd::Enqueue {
        kernels: state.kernels.clone(),
        kernel_id: kernel_id.to_string(),
        cells: cells.into_iter().map(|c| Selector::Id(c, None)).collect(),
        by,
        reply,
        finished: Some(finished),
    });
    let room = crate::collab::notebook_room_id(&state.collab, &state.contents, path)
        .await
        .map_err(|e| format!("cannot open {path}: {e}"))?;
    send_to_notebook_room(&state.collab, &state.contents, &room, cmd)
        .await
        .ok_or_else(|| format!("cannot open {path}"))?;
    match answer.await {
        Ok(Ok(_)) => {}
        Ok(Err(
            Refused::SourceMismatch(message)
            | Refused::Bad(message)
            | Refused::NotFound { message, .. },
        )) => return Err(message),
        Err(_) => return Err(format!("{path}: room closed")),
    }
    let mut done = done;
    if tokio::time::timeout(timeout, &mut done).await.is_ok() {
        return Ok(true);
    }
    tokio::spawn(async move {
        let _ = done.await;
        late.await;
    });
    Ok(false)
}

#[cfg(test)]
mod request_tests {
    use super::*;

    fn room_of(minor: u32) -> Room {
        let nb: ganymede_nbformat::Notebook = serde_json::from_value(json!({
            "cells": [
                {"cell_type": "code", "id": "a", "metadata": {"tags": ["t"]},
                 "source": "1", "outputs": [], "execution_count": null},
                {"cell_type": "markdown", "id": "m", "metadata": {"tags": ["t"]}, "source": "x"},
                {"cell_type": "code", "id": "b", "metadata": {"tags": "t"},
                 "source": "  \n", "outputs": [], "execution_count": null},
                {"cell_type": "code", "id": "c", "metadata": {"tags": ["u", "t"]},
                 "source": "2", "outputs": [], "execution_count": null}],
            "metadata": {}, "nbformat": 4, "nbformat_minor": minor
        }))
        .unwrap();
        Room::notebook_room(&nb, "a.ipynb")
    }

    fn enq(room: &mut Room, sels: &[Selector]) -> (Result<Vec<String>, Refused>, Effects) {
        enqueue(room, sels, "tester", &HashMap::new())
    }

    fn id(s: &str) -> Selector {
        Selector::Id(s.into(), None)
    }

    #[test]
    fn a_read_only_room_queues_nothing() {
        let mut room = room_of(5);
        assert!(enq(&mut room, &[id("a")]).0.is_ok());
        room.set_read_only(true);
        for sel in [id("a"), Selector::Tag("t".into()), Selector::All] {
            let (r, fx) = enq(&mut room, &[sel]);
            assert!(
                matches!(&r, Err(Refused::Bad(m)) if m.contains("read-only")),
                "{:?}",
                r.err().map(|_| ())
            );
            assert!(!fx.changed);
        }
    }

    #[test]
    fn selectors_resolve_in_room_order() {
        let mut room = room_of(5);
        let mut ids = |sels: &[Selector]| enq(&mut room, sels).0.unwrap();
        // Tags: code cells only, a non-list `tags` ignored.
        assert_eq!(ids(&[Selector::Tag("t".into())]), ["a", "c"]);
        // A tag skips a cell already queued; an id given again runs again.
        assert_eq!(
            ids(&[id("c"), Selector::Tag("t".into()), id("a")]),
            ["c", "a", "a"]
        );
        assert_eq!(ids(&[id("a"), id("a")]), ["a", "a"]);
        assert_eq!(ids(&[Selector::Tag("t".into()), Selector::All]), ["a", "c"]);
        // All: code cells with source, not blank ones or markdown.
        assert_eq!(ids(&[Selector::All]), ["a", "c"]);
    }

    #[test]
    fn a_refused_batch_changes_nothing() {
        let mut room = room_of(5);
        let (r, fx) = enq(&mut room, &[id("a"), id("zz")]);
        assert!(
            matches!(&r, Err(Refused::NotFound { cell_id: Some(c), message, .. })
            if c == "zz" && !message.contains("4.4"))
        );
        assert!(!fx.changed);
        let (r, fx) = enq(&mut room, &[id("a"), Selector::Tag("none".into())]);
        assert!(matches!(&r, Err(Refused::NotFound { tag: Some(t), .. }) if t == "none"));
        assert!(!fx.changed);
        let (r, fx) = enq(
            &mut room,
            &[id("a"), Selector::Id("c".into(), Some("0".into()))],
        );
        assert!(matches!(&r, Err(Refused::SourceMismatch(c)) if c == "c"));
        assert!(!fx.changed);
        let (r, fx) = enq(&mut room, &[id("m")]);
        assert!(matches!(&r, Err(Refused::Bad(m)) if m.contains("not a code cell")));
        assert!(!fx.changed);
        // The 4.4 hint.
        let mut old = room_of(4);
        let (r, _) = enq(&mut old, &[id("zz")]);
        assert!(
            matches!(&r, Err(Refused::NotFound { message, .. }) if message.contains("nbformat 4.4"))
        );
    }

    #[test]
    fn cells_parse_into_selectors() {
        let ok = |v: Value| parse_cells(Some(v)).unwrap_or_else(|_| panic!("refused"));
        assert_eq!(ok(json!("all")), [Selector::All]);
        assert_eq!(
            ok(
                json!([{"cell_id": "a"}, {"cell_id": "b", "source_hash": null},
                      {"cell_id": "c", "source_hash": 5}, {"cell_id": "d", "source_hash": "9"},
                      {"tag": "t"}])
            ),
            [
                id("a"),
                id("b"),
                Selector::Id("c".into(), Some("5".into())),
                Selector::Id("d".into(), Some("9".into())),
                Selector::Tag("t".into())
            ]
        );
        for bad in [
            json!("some"),
            json!(3),
            json!([]),
            json!([{}]),
            json!(["a"]),
            json!([{"cell_id": "a", "tag": "t"}]),
            json!([{"cell_id": 1}]),
            json!([{"tag": 1}]),
            json!([{"cell_id": "a", "source_hash": true}]),
        ] {
            assert!(parse_cells(Some(bad.clone())).is_err(), "{bad}");
        }
        assert!(parse_cells(None).is_err());
    }

    #[test]
    fn live_outputs_append_until_something_may_change_them() {
        // The calls follow what the handler does for each message:
        // `cleared` for every clear, `next` when the running cell changed.
        let mut l = LiveOutputs::default();
        // The first update resends: the room may show older outputs.
        assert_eq!(l.next(false, false, 1), None);
        // Streams and results add outputs: each sends only the new ones.
        assert_eq!(l.next(false, false, 2), Some(1));
        assert_eq!(l.next(false, false, 3), Some(2));
        // A display with an id may update earlier outputs.
        assert_eq!(l.next(false, true, 4), None);
        assert_eq!(l.next(false, false, 5), Some(4));
        // clear_output(wait) changes no output, so only `cleared`; the
        // next output clears and adds one: same count as before for one
        // output, so only the resync flag can tell.
        l.cleared();
        assert_eq!(l.next(false, false, 1), None);
        l.cleared();
        assert_eq!(l.next(false, false, 1), None);
        assert_eq!(l.next(false, false, 2), Some(1));
        // clear_output without wait: cleared now; the next one resends too.
        l.cleared();
        assert_eq!(l.next(true, false, 0), None);
        assert_eq!(l.next(false, false, 1), None);
        assert_eq!(l.next(false, false, 2), Some(1));
        // Fewer outputs than sent, without a clear seen: resend.
        assert_eq!(l.next(false, false, 1), None);
        assert_eq!(l.next(false, false, 2), Some(1));
    }

    #[test]
    fn queued_ids_outlive_the_minute_until_a_successor_waits() {
        let r = Requests::default();
        r.mark("queued");
        let _ = r.channel("never-queued");
        let old = Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("an old instant");
        for (_, t) in r.queued.lock().unwrap().values_mut() {
            *t = old;
        }
        // A later request prunes: the unqueued id goes, the queued stays.
        assert!(*r.channel("queued").borrow());
        assert!(!r.queued.lock().unwrap().contains_key("never-queued"));
        r.consumed("queued");
        assert!(r.queued.lock().unwrap().is_empty());
    }

    fn exec() -> RoomExec {
        RoomExec::new(mpsc::unbounded_channel().0)
    }

    /// A cell's `(execution_state, execution_by)` as the room has them.
    fn shown(room: &mut Room, id: &str) -> (Option<String>, Option<String>) {
        let mut out = (None, None);
        room.edit(|txn, roots| {
            let cell = schema::find_cell(txn, roots, id).unwrap();
            let text = |key: &str| match cell.get(txn, key) {
                Some(Out::Any(Any::String(s))) => Some(s.to_string()),
                _ => None,
            };
            out = (text("execution_state"), text("execution_by"));
        });
        out
    }

    fn state(room: &mut Room, id: &str) -> (&'static str, Option<String>) {
        let (s, by) = shown(room, id);
        let s = match s.as_deref() {
            Some("running") => "running",
            Some("idle") => "idle",
            other => panic!("state {other:?}"),
        };
        (s, by)
    }

    fn run(x: &mut RoomExec, room: &mut Room, sels: &[Selector], by: &str) {
        assert!(x.queue(room, sels, by).0.is_ok());
    }

    #[test]
    fn a_queued_cell_shows_who_runs_it_until_it_is_done() {
        let (mut room, mut x) = (room_of(5), exec());
        assert_eq!(state(&mut room, "a"), ("idle", None));
        run(&mut x, &mut room, &[id("a")], "Claude");
        assert_eq!(state(&mut room, "a"), ("running", Some("Claude".into())));
        // Another cell is not named.
        assert_eq!(state(&mut room, "c"), ("idle", None));
        x.finish(&mut room, "a", Some(Vec::new()), Some(1));
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert!(x.queued.is_empty());
    }

    #[test]
    fn a_cell_queued_twice_shows_the_run_in_progress() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        run(&mut x, &mut room, &[id("a")], "B");
        // B waits behind A: the name is still A's.
        assert_eq!(state(&mut room, "a"), ("running", Some("A".into())));
        x.finish(&mut room, "a", Some(Vec::new()), Some(1));
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
        x.finish(&mut room, "a", Some(Vec::new()), Some(2));
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert!(x.queued.is_empty());
    }

    #[test]
    fn the_next_run_marks_the_cell_running_again() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        run(&mut x, &mut room, &[id("a")], "B");
        // A client's own run of the cell marked it idle meanwhile.
        room.edit(|txn, roots| {
            let cell = schema::find_cell(txn, roots, "a").unwrap();
            cell.insert(txn, "execution_state", "idle");
        });
        x.finish(&mut room, "a", Some(Vec::new()), Some(1));
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
    }

    #[test]
    fn a_cell_given_twice_in_a_batch_runs_twice_before_it_is_idle() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a"), id("a")], "A");
        x.finish(&mut room, "a", Some(Vec::new()), Some(1));
        assert_eq!(state(&mut room, "a"), ("running", Some("A".into())));
        x.finish(&mut room, "a", Some(Vec::new()), Some(2));
        assert_eq!(state(&mut room, "a"), ("idle", None));
    }

    #[test]
    fn a_skipped_cell_ends_its_run_too() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        // Skipped after an earlier cell failed: no outputs, no count.
        x.finish(&mut room, "a", None, None);
        assert_eq!(state(&mut room, "a"), ("idle", None));
        // A later run is credited to its own runner, not the skipped one's.
        run(&mut x, &mut room, &[id("a")], "B");
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
    }

    #[test]
    fn a_deleted_cell_still_ends_its_run() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(&mut room, "zz", None, None);
        assert!(x.queued.contains_key("a"));
        // Done for an id the room no longer has pops its queue.
        run(&mut x, &mut room, &[id("c")], "A");
        room.edit(|txn, roots| {
            let n = roots.cells.len(txn);
            for i in (0..n).rev() {
                if let Some(Out::YMap(m)) = roots.cells.get(txn, i) {
                    if matches!(m.get(txn, "id"), Some(Out::Any(Any::String(s))) if &*s == "c") {
                        roots.cells.remove(txn, i);
                    }
                }
            }
        });
        x.finish(&mut room, "c", Some(Vec::new()), None);
        assert!(!x.queued.contains_key("c"));
    }

    #[test]
    fn a_refused_batch_names_no_one_and_queues_nothing() {
        let (mut room, mut x) = (room_of(5), exec());
        assert!(x.queue(&mut room, &[id("a"), id("zz")], "A").0.is_err());
        assert!(x
            .queue(
                &mut room,
                &[id("a"), Selector::Id("c".into(), Some("0".into()))],
                "A"
            )
            .0
            .is_err());
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert!(x.queued.is_empty());
    }

    #[test]
    fn the_runner_is_not_saved_or_listed() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "Claude");
        assert_eq!(state(&mut room, "a"), ("running", Some("Claude".into())));
        let nb = room.notebook_with_ids().unwrap().unwrap();
        let text = serde_json::to_string(&nb).unwrap();
        assert!(
            !text.contains("execution_by") && !text.contains("Claude"),
            "{text}"
        );
        assert!(!text.contains("execution_state"), "{text}");
    }

    #[test]
    fn the_name_of_a_rest_runner() {
        // A vouched login wins over the body's name.
        assert_eq!(runner_name(Some("ann@x.com"), Some("bot")), "ann@x.com");
        assert_eq!(runner_name(None, Some("bot")), "bot");
        assert_eq!(runner_name(None, None), API_CLIENT);
        // Cleaned like a room client's name.
        assert_eq!(runner_name(None, Some("  a\u{7}b \n  c ")), "ab c");
        assert_eq!(runner_name(None, Some(&"x".repeat(100))).len(), 64);
        // Nothing left: the next source.
        assert_eq!(runner_name(None, Some(" \t\u{7} ")), API_CLIENT);
        assert_eq!(runner_name(Some(" "), Some("bot")), "bot");
        assert_eq!(runner_name(Some(" "), Some("")), API_CLIENT);
    }
}
