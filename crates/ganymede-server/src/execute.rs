//! Server-side execution into a document room: `jupyter-server-documents`'
//! `POST /api/kernels/{kernel_id}/execute` (#244).
//!
//! ```text
//! {"document_id": "json:notebook:<file id>",
//!  "cells": [{"cell_id": "...", "source_hash": "<MurmurHash2, seed 0>"}],
//!  "client_id": "...", "request_id": "...", "previous_request_id": "..."}
//! ```
//!
//! The room checks every cell's hash against its current source, then
//! clears their outputs, marks them running, and queues the batch, all in
//! one step of its task, so no other request interleaves with it. The
//! answer is `200 null` once queued; `409 {"error": "source_mismatch",
//! "cell_id"}` when a cell's source changed since the client hashed it;
//! `408` when `previous_request_id` was not queued within ten seconds;
//! `400` for the rest. A room that isn't open is opened, so an agent with
//! no websocket can run cells.
//!
//! One worker per room runs the queue in order on the named kernel,
//! through `ganymede-exec`'s [`NotebookRun`] (display updates, clears,
//! counts), and writes outputs into the room as they arrive, so they land
//! with no client connected and are saved with the notebook. A cell that
//! fails stops the rest of its batch, as a kernel's `stop_on_error` does for
//! JupyterLab's Run All. Input requests are refused (`allow_stdin` off).

use crate::collab::{send_to_notebook_room, Cmd};
use crate::routes::{HttpError, LenientJson};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use ganymede_api::KernelManager;
use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::{Any, GetString, Map, Out};
use ganymede_documents::{Effects, Room};
use ganymede_exec::{CellStatus, Event, NotebookRun, Options};
use ganymede_nbformat::Cell;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
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

#[derive(Deserialize, Default)]
pub struct ExecuteBody {
    document_id: Option<String>,
    cells: Option<Value>,
    #[allow(dead_code)]
    client_id: Option<String>,
    request_id: Option<String>,
    previous_request_id: Option<String>,
}

/// Why a batch was refused.
#[derive(Debug)]
pub(crate) enum Refused {
    SourceMismatch(String),
    Bad(String),
}

/// What the room task is asked, by the endpoint and by its worker.
pub(crate) enum RoomCmd {
    /// Check and queue a batch.
    Enqueue {
        kernels: Arc<KernelManager>,
        kernel_id: String,
        /// Cell ids, with the source hash to check (`None`: don't check).
        cells: Vec<(String, Option<String>)>,
        reply: oneshot::Sender<Result<(), Refused>>,
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
}

impl RoomExec {
    pub(crate) fn new(me: mpsc::UnboundedSender<Cmd>) -> Self {
        RoomExec {
            me,
            worker: None,
            pending: 0,
        }
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
                reply,
                finished,
            } => {
                let (checked, fx) = enqueue(room, &cells);
                if checked.is_ok() {
                    self.pending += cells.len();
                    let worker = self.worker.get_or_insert_with(|| {
                        let (tx, rx) = mpsc::unbounded_channel();
                        tokio::spawn(run_worker(self.me.clone(), rx));
                        tx
                    });
                    let _ = worker.send(Batch {
                        kernels,
                        kernel_id,
                        cells: cells.into_iter().map(|(id, _)| id).collect(),
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
            } => {
                self.pending = self.pending.saturating_sub(1);
                room.edit(|txn, roots| {
                    let Some(cell) = schema::find_cell(txn, roots, &cell_id) else {
                        return;
                    };
                    if let Some(outputs) = outputs {
                        schema::set_outputs(txn, &cell, outputs);
                    }
                    if let Some(n) = execution_count {
                        cell.insert(txn, "execution_count", schema::json_to_any(&json!(n)));
                    }
                    cell.insert(txn, "execution_state", "idle");
                })
                .1
            }
        }
    }
}

/// Check every cell, then clear and mark them all, or none.
fn enqueue(room: &mut Room, cells: &[(String, Option<String>)]) -> (Result<(), Refused>, Effects) {
    if room.notebook().is_none() {
        return (
            Err(Refused::Bad("not a notebook room".into())),
            Effects::default(),
        );
    }
    room.edit(|txn, roots: &Roots| {
        let mut found = Vec::new();
        for (id, hash) in cells {
            let Some(cell) = schema::find_cell(txn, roots, id) else {
                return Err(Refused::Bad(format!("Cell {id:?} not found in document")));
            };
            let code = matches!(cell.get(txn, "cell_type"), Some(Out::Any(Any::String(t))) if &*t == "code");
            if !code {
                return Err(Refused::Bad(format!("Cell {id:?} is not a code cell")));
            }
            let source = match cell.get(txn, "source") {
                Some(Out::YText(t)) => t.get_string(txn),
                Some(Out::Any(Any::String(s))) => s.to_string(),
                _ => String::new(),
            };
            let stale = hash
                .as_ref()
                .is_some_and(|h| ganymede_documents::murmur2(source.as_bytes(), 0).to_string() != *h);
            if stale {
                return Err(Refused::SourceMismatch(id.clone()));
            }
            found.push(cell);
        }
        for cell in found {
            schema::set_outputs(txn, &cell, Vec::new());
            cell.insert(txn, "execution_state", "running");
        }
        Ok(())
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

pub async fn execute(
    State(state): State<AppState>,
    Path(kernel_id): Path<String>,
    LenientJson(body): LenientJson<ExecuteBody>,
) -> Result<Response, HttpError> {
    let document_id = body
        .document_id
        .filter(|d| !d.is_empty())
        .ok_or_else(|| bad("document_id is required"))?;
    let entries = match body.cells {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => {
            return Err(bad(
                "cells must be a non-empty list of {cell_id, source_hash}",
            ))
        }
    };
    let mut cells = Vec::new();
    for entry in &entries {
        let id = entry
            .get("cell_id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                bad(format!(
                    "Each cell entry must have a cell_id string, got {entry}"
                ))
            })?;
        let hash = match entry.get("source_hash") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err(bad(format!("source_hash is required for cell {id:?}"))),
        };
        cells.push((id.to_string(), Some(hash)));
    }
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
    let cmd = Cmd::Exec(RoomCmd::Enqueue {
        kernels: state.kernels.clone(),
        kernel_id,
        cells,
        reply,
        finished: None,
    });
    send_to_notebook_room(&state.collab, &state.contents, &document_id, cmd)
        .await
        .ok_or_else(|| bad(format!("No YRoom available for document: {document_id:?}")))?;
    match answer.await {
        Ok(Ok(())) => {}
        Ok(Err(Refused::SourceMismatch(cell_id))) => {
            return Ok((
                StatusCode::CONFLICT,
                axum::Json(json!({"error": "source_mismatch", "cell_id": cell_id})),
            )
                .into_response())
        }
        Ok(Err(Refused::Bad(msg))) => return Err(bad(msg)),
        Err(_) => return Err(bad(format!("Room {document_id:?} closed"))),
    }
    if let Some(id) = body.request_id.as_deref().filter(|p| !p.is_empty()) {
        state.collab.requests.mark(id);
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "null",
    )
        .into_response())
}

/// Queue cells of the notebook at `path` on `kernel_id`, unchecked, and
/// wait until they have run or `timeout` passes. False on a timeout.
pub(crate) async fn run_and_wait(
    state: &AppState,
    path: &str,
    kernel_id: &str,
    cells: Vec<String>,
    timeout: Duration,
) -> Result<bool, String> {
    let (reply, answer) = oneshot::channel();
    let (finished, done) = oneshot::channel();
    let cmd = Cmd::Exec(RoomCmd::Enqueue {
        kernels: state.kernels.clone(),
        kernel_id: kernel_id.to_string(),
        cells: cells.into_iter().map(|c| (c, None)).collect(),
        reply,
        finished: Some(finished),
    });
    let room = crate::collab::notebook_room_id(&state.collab, path).await;
    send_to_notebook_room(&state.collab, &state.contents, &room, cmd)
        .await
        .ok_or_else(|| format!("cannot open {path}"))?;
    match answer.await {
        Ok(Ok(())) => {}
        Ok(Err(Refused::SourceMismatch(id) | Refused::Bad(id))) => return Err(id),
        Err(_) => return Err(format!("{path}: room closed")),
    }
    Ok(tokio::time::timeout(timeout, done).await.is_ok())
}

#[cfg(test)]
mod request_tests {
    use super::*;

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
}
