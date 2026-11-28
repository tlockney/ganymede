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
//! request interleaves with it. A hash is a guard that travels with the
//! queued cell (#943): the worker checks it again when the cell's turn
//! comes, against the source the room has then, and a cell that changed
//! while it waited is not run. A cell with no hash, a tag or `"all"` runs
//! the source it has at its turn. The answer is `200 null` once queued;
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
//! `stop_on_error` does for JupyterLab's Run All. A cell that ran gets
//! `metadata.ganymede.ran_source_hash`, the hash of the source it ran,
//! saved with the notebook. Only these runs write it: room runs
//! (this module), `POST /api/notebooks/run` and the host API. A client
//! update that sets a cell's `execution_count` to another value, or
//! empties outputs it had, is the client running or clearing it, and
//! the room removes it (and `execution_skipped`), since the server
//! doesn't see what source ran. An update that only adds to or rewrites
//! outputs that stay non-empty, as a client echoing a server run's
//! outputs does, keeps it. It is cell metadata because
//! stream and error outputs have none of their own. Input requests are refused (`allow_stdin`
//! off).

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
        /// Told when the whole batch has run: whether every cell ended
        /// without a failure (a cell that went before it ran doesn't fail
        /// it), and which cells changed while queued.
        finished: Option<oneshot::Sender<BatchEnd>>,
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
        /// The hash of the source that ran, for a cell that ran.
        ran_source_hash: Option<String>,
        /// The cell was not run: its source changed after its hash was
        /// checked (#943).
        changed: bool,
    },
}

/// How a batch ended.
#[derive(Debug)]
pub(crate) struct BatchEnd {
    /// No cell failed or was cut short.
    pub all_ok: bool,
    /// The cells that were not run because their source changed while
    /// they were queued, in the order they were to run.
    pub changed: Vec<String>,
}

/// A queued cell: its id, and the hash its source must still have when
/// its turn comes (`None`: run whatever it holds).
type Queued = (String, Option<String>);

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
    cells: Vec<Queued>,
    finished: Option<oneshot::Sender<BatchEnd>>,
}

/// A room's side of execution: its queue and how many cells are pending.
pub(crate) struct RoomExec {
    me: mpsc::UnboundedSender<Cmd>,
    worker: Option<mpsc::UnboundedSender<Batch>>,
    pending: usize,
    /// Per cell id, each queued or running run of it, in order: the front
    /// is the run in progress. Entries are dropped when empty.
    queued: HashMap<String, VecDeque<QueuedRun>>,
    /// The number of the next queued batch.
    batches: u64,
}

/// One queued run of a cell.
struct QueuedRun {
    /// Who runs it.
    by: String,
    /// The batch it is in: a change stops the rest of that batch only.
    batch: u64,
    /// It was not run because an earlier run of the cell in its batch was
    /// skipped for a change, which stops the batch. The skip note goes by
    /// the cell's last run alone.
    changed: bool,
}

impl RoomExec {
    pub(crate) fn new(me: mpsc::UnboundedSender<Cmd>) -> Self {
        RoomExec {
            me,
            worker: None,
            pending: 0,
            queued: HashMap::new(),
            batches: 0,
        }
    }

    /// Check and queue `selectors` for `by`: [`enqueue`], then remember
    /// who runs each cell, in the same step of the room's task.
    fn queue(
        &mut self,
        room: &mut Room,
        selectors: &[Selector],
        by: &str,
    ) -> (Result<Vec<Queued>, Refused>, Effects) {
        let (checked, fx) = enqueue(room, selectors, by, &self.queued);
        if let Ok(ids) = &checked {
            let batch = self.batches;
            self.batches += 1;
            for (id, _) in ids {
                self.queued
                    .entry(id.clone())
                    .or_default()
                    .push_back(QueuedRun {
                        by: by.to_string(),
                        batch,
                        changed: false,
                    });
            }
        }
        (checked, fx)
    }

    /// A run of `cell_id` ended (or was skipped): its outputs, if it ran.
    /// The cell stays running, now by whoever queued next, while it has
    /// runs queued; otherwise it is idle and has no `execution_by`. A cell
    /// whose last queued run was skipped because it `changed` while queued
    /// gets `execution_skipped`; a change also skips the cell's later runs
    /// in the same batch, which carry it. A cell that ran records
    /// `ran_source_hash`.
    fn finish(
        &mut self,
        room: &mut Room,
        cell_id: &str,
        outputs: Option<Vec<Value>>,
        execution_count: Option<i64>,
        ran_source_hash: Option<String>,
        changed: bool,
    ) -> Effects {
        self.pending = self.pending.saturating_sub(1);
        // Also when the cell is gone, so a later cell of that id starts
        // with no stale name.
        let mut note = false;
        let next = match self.queued.get_mut(cell_id) {
            Some(runs) => {
                let ended = runs.pop_front();
                let changed = changed || ended.as_ref().is_some_and(|r| r.changed);
                if let Some(ended) = ended.filter(|_| changed) {
                    // The batch stops: its next run of this cell is skipped
                    // for the same reason.
                    if let Some(later) = runs.iter_mut().find(|r| r.batch == ended.batch) {
                        later.changed = true;
                    }
                }
                let next = runs.front().map(|r| r.by.clone());
                if next.is_none() {
                    self.queued.remove(cell_id);
                    note = changed;
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
            // Not on a cell that is no longer a code cell.
            let code = cell
                .get(txn, "cell_type")
                .map(|o| schema::out_to_json(txn, &o))
                == Some(json!("code"));
            if let Some(hash) = ran_source_hash.as_ref().filter(|_| code) {
                schema::set_ran_source_hash(txn, &cell, hash);
            }
            if note {
                cell.insert(txn, "execution_skipped", CHANGED_WHILE_QUEUED);
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
                let Ok(queued) = checked else {
                    let _ = reply.send(checked.map(|_| Vec::new()));
                    return fx;
                };
                let ids = queued.iter().map(|(id, _)| id.clone()).collect();
                if queued.is_empty() {
                    if let Some(f) = finished {
                        let _ = f.send(BatchEnd {
                            all_ok: true,
                            changed: Vec::new(),
                        });
                    }
                    let _ = reply.send(Ok(ids));
                    return fx;
                }
                self.pending += queued.len();
                let worker = self.worker.get_or_insert_with(|| {
                    let (tx, rx) = mpsc::unbounded_channel();
                    tokio::spawn(run_worker(self.me.clone(), rx));
                    tx
                });
                let _ = worker.send(Batch {
                    kernels,
                    kernel_id,
                    cells: queued,
                    finished,
                });
                let _ = reply.send(Ok(ids));
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
                ran_source_hash,
                changed,
            } => self.finish(
                room,
                &cell_id,
                outputs,
                execution_count,
                ran_source_hash,
                changed,
            ),
        }
    }
}

/// Resolve the selectors against the room's cells, check them, then clear
/// and mark every cell, or none. The ids come back in run order, each with
/// the hash it was checked against, if it was given one (a cell chosen by
/// tag or `"all"` has none).
///
/// A cell with no run queued gets `execution_by` = `by`; one already
/// queued (`queued`) keeps the name of the run in progress.
fn enqueue(
    room: &mut Room,
    selectors: &[Selector],
    by: &str,
    queued: &HashMap<String, VecDeque<QueuedRun>>,
) -> (Result<Vec<Queued>, Refused>, Effects) {
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
        let mut found: Vec<(String, MapRef, Option<String>)> = Vec::new();
        // A tag or "all" skips a cell already queued; an id given twice
        // runs twice, as in jupyter-server-documents.
        let mut add = |id: &str, cell: &MapRef, again: bool, guard: Option<String>| {
            if again || !found.iter().any(|(f, _, _)| f == id) {
                found.push((id.to_string(), cell.clone(), guard));
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
                        ganymede_nbformat::source_hash(&source) != *h
                    });
                    if stale {
                        return Err(Refused::SourceMismatch(id.clone()));
                    }
                    add(id, &cell, true, hash.clone());
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
                            add(id, cell, false, None);
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
                            add(id, cell, false, None);
                        }
                    }
                }
            }
        }
        for (id, cell, _) in &found {
            schema::set_outputs(txn, cell, Vec::new());
            // The outputs are gone, so no source "ran" to give them: only a
            // run that ends puts the hash back.
            schema::clear_ran_source_hash(txn, cell);
            cell.insert(txn, "execution_state", "running");
            cell.remove(txn, "execution_skipped");
            if queued.get(id).is_none_or(VecDeque::is_empty) {
                cell.insert(txn, "execution_by", by);
            }
        }
        Ok(found.into_iter().map(|(id, _, g)| (id, g)).collect())
    })
}

/// Runs a room's batches in order.
async fn run_worker(room: mpsc::UnboundedSender<Cmd>, mut rx: mpsc::UnboundedReceiver<Batch>) {
    let mut runs: HashMap<String, Run> = HashMap::new();
    while let Some(batch) = rx.recv().await {
        let mut stopped = false;
        let mut changed = Vec::new();
        for (cell_id, guard) in batch.cells {
            if stopped {
                done(&room, cell_id, None, None);
                continue;
            }
            let run = runs.entry(batch.kernel_id.clone()).or_insert_with(Run::new);
            match run
                .cell(
                    &room,
                    &batch.kernels,
                    &batch.kernel_id,
                    cell_id.clone(),
                    guard,
                )
                .await
            {
                CellEnd::Ok => {}
                CellEnd::Failed => stopped = true,
                CellEnd::Changed => {
                    stopped = true;
                    changed.push(cell_id);
                }
            }
        }
        if let Some(f) = batch.finished {
            let _ = f.send(BatchEnd {
                all_ok: !stopped,
                changed,
            });
        }
    }
}

/// What a cell's turn came to.
enum CellEnd {
    /// It ran without failing, or was deleted while queued.
    Ok,
    /// It failed, or could not be run: the batch stops.
    Failed,
    /// Its source no longer had the hash it was queued with: it was not
    /// run, and the batch stops.
    Changed,
}

/// Shown on a cell that was not run because its source changed.
const CHANGED_WHILE_QUEUED: &str = "Cell changed while queued";

/// A cell that was deleted, or is no longer a code cell, when its turn
/// came. One queued with a hash (`guarded`) is one whose source the caller
/// named: not running it stops the batch, as a change does, and the room
/// says so if the cell is still there.
fn not_code_end(room: &mpsc::UnboundedSender<Cmd>, cell_id: String, guarded: bool) -> CellEnd {
    send_done(room, cell_id, None, None, None, guarded);
    if guarded {
        CellEnd::Changed
    } else {
        CellEnd::Ok
    }
}

/// The hash to record for a run: only when the kernel took the source
/// (`execute_input` gave a count).
fn hash_if_ran(count: Option<i64>, ran_hash: String) -> Option<String> {
    count.map(|_| ran_hash)
}

/// A cell that was skipped or could not run: no outputs, no count.
fn done(
    room: &mpsc::UnboundedSender<Cmd>,
    cell_id: String,
    outputs: Option<Vec<Value>>,
    execution_count: Option<i64>,
) {
    send_done(room, cell_id, outputs, execution_count, None, false);
}

fn send_done(
    room: &mpsc::UnboundedSender<Cmd>,
    cell_id: String,
    outputs: Option<Vec<Value>>,
    execution_count: Option<i64>,
    ran_source_hash: Option<String>,
    changed: bool,
) {
    let _ = room.send(Cmd::Exec(RoomCmd::Done {
        cell_id,
        outputs,
        execution_count,
        ran_source_hash,
        changed,
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

    /// Run one cell, asking the room for its source now. With a `guard`
    /// that the source no longer matches, the cell is not run (#943).
    async fn cell(
        &mut self,
        room: &mpsc::UnboundedSender<Cmd>,
        kernels: &KernelManager,
        kernel_id: &str,
        cell_id: String,
        guard: Option<String>,
    ) -> CellEnd {
        let kernel = match kernels.wait_started(kernel_id).await {
            Ok(_) => kernels.get(kernel_id),
            Err(e) => Err(e),
        };
        let kernel = match kernel {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(kernel = %kernel_id, "cannot run cell {cell_id}: {}", e.message);
                done(room, cell_id, None, None);
                return CellEnd::Failed;
            }
        };
        let (tx, rx) = oneshot::channel();
        let _ = room.send(Cmd::Exec(RoomCmd::Get(cell_id.clone(), tx)));
        let Ok(Some(Cell::Code(mut cell))) = rx.await else {
            return not_code_end(room, cell_id, guard.is_some());
        };
        let ran_hash = ganymede_nbformat::source_hash(&cell.source);
        if guard.is_some_and(|g| g != ran_hash) {
            send_done(room, cell_id, None, None, None, true);
            return CellEnd::Changed;
        }
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
        // No `execute_input` means the kernel never ran the source: clear
        // the hash, as for a cell that was not run.
        send_done(
            room,
            cell_id,
            outputs,
            count,
            hash_if_ran(count, ran_hash),
            false,
        );
        if ok {
            CellEnd::Ok
        } else {
            CellEnd::Failed
        }
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
        let ended = tokio::time::timeout(wait, done)
            .await
            .ok()
            .and_then(Result::ok);
        let cells = room_cells(&room, ids).await;
        let mut answer = json!({"finished": ended.is_some(), "cells": cells});
        let skipped: Vec<Value> = ended
            .iter()
            .flat_map(|e| &e.changed)
            .map(|id| {
                json!({"cell_id": id, "error": "source_changed_while_queued",
                       "message": CHANGED_WHILE_QUEUED})
            })
            .collect();
        if !skipped.is_empty() {
            answer["skipped"] = Value::Array(skipped);
        }
        return Ok(axum::Json(answer).into_response());
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

/// How [`run_and_wait`] ended.
pub(crate) struct RunEnd {
    /// The cells all ran before the timeout.
    pub finished: bool,
    /// Finished, and no cell failed or was cut short: an error, a reply
    /// of `aborted`, a kernel that went, or one that couldn't be reached.
    pub all_ok: bool,
}

/// Queue cells of the notebook at `path` on `kernel_id`, unchecked, and
/// wait until they have run or `timeout` passes. `finished` is false on a
/// timeout; then `late` runs once they have all run (or the room stops),
/// in a task of its own.
pub(crate) async fn run_and_wait(
    state: &AppState,
    path: &str,
    kernel_id: &str,
    cells: Vec<String>,
    by: String,
    timeout: Duration,
    late: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<RunEnd, String> {
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
    if let Ok(ended) = tokio::time::timeout(timeout, &mut done).await {
        return Ok(RunEnd {
            finished: true,
            all_ok: ended.is_ok_and(|e| e.all_ok),
        });
    }
    tokio::spawn(async move {
        let _ = done.await;
        late.await;
    });
    Ok(RunEnd {
        finished: false,
        all_ok: false,
    })
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
        let (r, fx) = enqueue(room, sels, "tester", &HashMap::new());
        (r.map(|q| q.into_iter().map(|(id, _)| id).collect()), fx)
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
        x.finish(&mut room, "a", Some(Vec::new()), Some(1), None, false);
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
        x.finish(&mut room, "a", Some(Vec::new()), Some(1), None, false);
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
        x.finish(&mut room, "a", Some(Vec::new()), Some(2), None, false);
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
        x.finish(&mut room, "a", Some(Vec::new()), Some(1), None, false);
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
    }

    #[test]
    fn a_cell_given_twice_in_a_batch_runs_twice_before_it_is_idle() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a"), id("a")], "A");
        x.finish(&mut room, "a", Some(Vec::new()), Some(1), None, false);
        assert_eq!(state(&mut room, "a"), ("running", Some("A".into())));
        x.finish(&mut room, "a", Some(Vec::new()), Some(2), None, false);
        assert_eq!(state(&mut room, "a"), ("idle", None));
    }

    #[test]
    fn a_skipped_cell_ends_its_run_too() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        // Skipped after an earlier cell failed: no outputs, no count.
        x.finish(&mut room, "a", None, None, None, false);
        assert_eq!(state(&mut room, "a"), ("idle", None));
        // A later run is credited to its own runner, not the skipped one's.
        run(&mut x, &mut room, &[id("a")], "B");
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
    }

    #[test]
    fn a_deleted_cell_still_ends_its_run() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(&mut room, "zz", None, None, None, false);
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
        x.finish(&mut room, "c", Some(Vec::new()), None, None, false);
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

    /// A cell's text field as the room has it.
    fn text_of(room: &mut Room, id: &str, key: &str) -> Option<String> {
        let mut out = None;
        room.edit(|txn, roots| {
            let cell = schema::find_cell(txn, roots, id).unwrap();
            out = match cell.get(txn, key) {
                Some(Out::Any(Any::String(s))) => Some(s.to_string()),
                _ => None,
            };
        });
        out
    }

    /// A cell as the notebook has it, runtime fields included.
    fn cell_json(room: &mut Room, id: &str) -> Value {
        let nb = room.notebook_with_ids().unwrap().unwrap();
        let cells = serde_json::to_value(&nb).unwrap()["cells"].clone();
        cells
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()
            .clone()
    }

    #[test]
    fn a_guard_travels_with_an_id_and_only_an_id() {
        let mut room = room_of(5);
        let hash = ganymede_nbformat::source_hash("1");
        let (r, _) = enqueue(
            &mut room,
            &[
                Selector::Id("a".into(), Some(hash.clone())),
                id("c"),
                Selector::Tag("t".into()),
            ],
            "tester",
            &HashMap::new(),
        );
        // The tag adds nothing: a and c are queued already.
        assert_eq!(
            r.ok().unwrap(),
            [("a".to_string(), Some(hash)), ("c".to_string(), None)]
        );
        let (r, _) = enqueue(&mut room, &[Selector::All], "tester", &HashMap::new());
        assert_eq!(
            r.ok().unwrap(),
            [("a".to_string(), None), ("c".to_string(), None)]
        );
    }

    #[test]
    fn a_cell_that_changed_while_queued_shows_why_it_did_not_run() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a"), id("c")], "A");
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
        x.finish(&mut room, "a", None, None, None, true);
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert_eq!(
            text_of(&mut room, "a", "execution_skipped").as_deref(),
            Some("Cell changed while queued")
        );
        // A cell skipped after another's failure or change has no note.
        x.finish(&mut room, "c", None, None, None, false);
        assert_eq!(text_of(&mut room, "c", "execution_skipped"), None);
        // Queued again, the note goes.
        run(&mut x, &mut room, &[id("a")], "A");
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
    }

    #[test]
    fn a_cell_queued_again_shows_no_note_while_its_next_run_waits() {
        // `[a#H, a#H]` with `a` changed: the worker reports the first run
        // changed, stops the batch, and skips the second with no change.
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a"), id("a")], "A");
        x.finish(&mut room, "a", None, None, None, true);
        assert_eq!(state(&mut room, "a"), ("running", Some("A".into())));
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
        x.finish(&mut room, "a", None, None, None, false);
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert_eq!(
            text_of(&mut room, "a", "execution_skipped").as_deref(),
            Some("Cell changed while queued")
        );
        // The memory is spent: a later plain skip shows nothing.
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(&mut room, "a", None, None, None, false);
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
    }

    #[test]
    fn a_cell_skipped_for_a_change_then_for_a_failure_shows_no_change_note() {
        // A queues `[a#H]`, B queues `[c, a]`. `a` changed, so A's run is
        // skipped; `c` fails, so B's run of `a` is skipped for that.
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        run(&mut x, &mut room, &[id("c"), id("a")], "B");
        x.finish(&mut room, "a", None, None, None, true);
        assert_eq!(state(&mut room, "a"), ("running", Some("B".into())));
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
        x.finish(&mut room, "c", None, None, None, false);
        x.finish(&mut room, "a", None, None, None, false);
        assert_eq!(state(&mut room, "a"), ("idle", None));
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
    }

    #[test]
    fn a_later_run_that_ran_leaves_no_skip_note() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        run(&mut x, &mut room, &[id("a")], "B");
        x.finish(&mut room, "a", None, None, None, true);
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(1),
            Some("7".into()),
            false,
        );
        assert_eq!(text_of(&mut room, "a", "execution_skipped"), None);
    }

    #[test]
    fn the_ran_hash_is_recorded_only_when_the_kernel_took_the_source() {
        assert_eq!(hash_if_ran(None, "h".into()), None);
        assert_eq!(hash_if_ran(Some(3), "h".into()), Some("h".into()));
    }

    #[test]
    fn a_cell_that_is_gone_or_not_code_ends_a_guarded_run_as_changed() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sent = |rx: &mut mpsc::UnboundedReceiver<Cmd>| match rx.try_recv() {
            Ok(Cmd::Exec(RoomCmd::Done { changed, .. })) => changed,
            _ => panic!("a Done"),
        };
        assert!(matches!(
            not_code_end(&tx, "a".into(), true),
            CellEnd::Changed
        ));
        assert!(sent(&mut rx));
        assert!(matches!(not_code_end(&tx, "a".into(), false), CellEnd::Ok));
        assert!(!sent(&mut rx));
    }

    #[test]
    fn a_cell_that_stopped_being_code_gets_no_hash() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        room.edit(|txn, roots| {
            let a = schema::find_cell(txn, roots, "a").unwrap();
            a.insert(txn, "cell_type", "markdown");
        });
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(1),
            Some("7".into()),
            false,
        );
        let has_hash = room
            .edit(|txn, roots| {
                let a = schema::find_cell(txn, roots, "a").unwrap();
                let meta = a.get(txn, "metadata").map(|m| schema::out_to_json(txn, &m));
                meta.is_some_and(|m| m["ganymede"]["ran_source_hash"].is_string())
            })
            .0;
        assert!(!has_hash);
    }

    #[test]
    fn a_cell_cleared_without_running_loses_its_hash() {
        // `[c, a]`: c fails, so a is skipped. Its outputs were cleared when
        // it was queued, so the hash of the run that made them goes too.
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(1),
            Some("77".into()),
            false,
        );
        assert_eq!(
            cell_json(&mut room, "a")["metadata"]["ganymede"]["ran_source_hash"],
            "77"
        );
        run(&mut x, &mut room, &[id("c"), id("a")], "A");
        x.finish(
            &mut room,
            "c",
            Some(Vec::new()),
            Some(2),
            Some("5".into()),
            false,
        );
        x.finish(&mut room, "a", None, None, None, false);
        assert!(cell_json(&mut room, "a")["metadata"]["ganymede"]
            .get("ran_source_hash")
            .is_none());
    }

    #[test]
    fn the_skip_note_is_not_saved_or_listed() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(&mut room, "a", None, None, None, true);
        assert!(text_of(&mut room, "a", "execution_skipped").is_some());
        let nb = room.notebook_with_ids().unwrap().unwrap();
        let text = serde_json::to_string(&nb).unwrap();
        assert!(!text.contains("execution_skipped"), "{text}");
        assert!(!text.contains("changed while queued"), "{text}");
    }

    #[test]
    fn a_cell_that_ran_records_the_hash_of_its_source() {
        let (mut room, mut x) = (room_of(5), exec());
        run(&mut x, &mut room, &[id("a")], "A");
        let hash = ganymede_nbformat::source_hash("1");
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(1),
            Some(hash.clone()),
            false,
        );
        let cell = cell_json(&mut room, "a");
        assert_eq!(cell["metadata"]["ganymede"]["ran_source_hash"], json!(hash));
        // Other metadata stays.
        assert_eq!(cell["metadata"]["tags"], json!(["t"]));
        // A skipped cell records none.
        run(&mut x, &mut room, &[id("c")], "A");
        x.finish(&mut room, "c", None, None, None, true);
        assert!(cell_json(&mut room, "c")["metadata"]["ganymede"].is_null());
    }

    #[test]
    fn the_hash_keeps_the_other_keys_of_the_cells_ganymede_metadata() {
        use ganymede_documents::yrs::MapPrelim;
        let (mut room, mut x) = (room_of(5), exec());
        // `a` has its `ganymede` as plain JSON; `c` as a shared map, as
        // a client writes it.
        room.edit(|txn, roots| {
            let a = schema::find_cell(txn, roots, "a").unwrap();
            let Some(Out::YMap(meta)) = a.get(txn, "metadata") else {
                panic!("metadata is a map");
            };
            meta.insert(
                txn,
                "ganymede",
                schema::json_to_any(&json!({"keep": [1, "x"]})),
            );
            let c = schema::find_cell(txn, roots, "c").unwrap();
            let Some(Out::YMap(meta)) = c.get(txn, "metadata") else {
                panic!("metadata is a map");
            };
            let g = meta.insert(txn, "ganymede", MapPrelim::default());
            g.insert(txn, "keep", "yes");
        });
        run(&mut x, &mut room, &[id("a"), id("c")], "A");
        for cell in ["a", "c"] {
            x.finish(
                &mut room,
                cell,
                Some(Vec::new()),
                Some(1),
                Some("77".into()),
                false,
            );
        }
        let a = cell_json(&mut room, "a");
        assert_eq!(
            a["metadata"]["ganymede"],
            json!({"keep": [1, "x"], "ran_source_hash": "77"})
        );
        let c = cell_json(&mut room, "c");
        assert_eq!(
            c["metadata"]["ganymede"],
            json!({"keep": "yes", "ran_source_hash": "77"})
        );
        // A second run replaces the hash.
        run(&mut x, &mut room, &[id("a")], "A");
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(2),
            Some("78".into()),
            false,
        );
        assert_eq!(
            cell_json(&mut room, "a")["metadata"]["ganymede"]["ran_source_hash"],
            "78"
        );
    }

    #[test]
    fn a_client_run_removes_the_hash_and_a_room_run_records_it_again() {
        use ganymede_documents::yrs::sync::{Message, SyncMessage};
        use ganymede_documents::yrs::updates::{decoder::Decode, encoder::Encode};
        use ganymede_documents::yrs::{Doc, ReadTxn, Transact, Update};
        let (mut room, mut x) = (room_of(5), exec());
        room.edit(|txn, roots| {
            let a = schema::find_cell(txn, roots, "a").unwrap();
            let Some(Out::YMap(meta)) = a.get(txn, "metadata") else {
                panic!("metadata is a map");
            };
            let keep = schema::json_to_any(&json!({"keep": 1}));
            meta.insert(txn, "ganymede", keep);
        });
        let room_run = |room: &mut Room, x: &mut RoomExec, hash: &str| {
            run(x, room, &[id("a")], "A");
            let out = json!({"output_type": "stream", "name": "stdout", "text": "1"});
            let outputs = Some(vec![out]);
            x.finish(room, "a", outputs, Some(1), Some(hash.into()), false);
            cell_json(room, "a")["metadata"]["ganymede"].clone()
        };
        let recorded = room_run(&mut room, &mut x, "77");
        assert_eq!(recorded, json!({"keep": 1, "ran_source_hash": "77"}));

        // A client runs it, as JupyterLab does: blank the count, replace
        // the outputs.
        let client = Doc::new();
        let state = Update::decode_v1(&room.encode_state()).unwrap();
        client.transact_mut().apply_update(state).unwrap();
        let before = client.transact().state_vector();
        {
            let roots = Roots::get(&client);
            let mut txn = client.transact_mut();
            let a = schema::find_cell(&txn, &roots, "a").unwrap();
            a.insert(&mut txn, "execution_count", Any::Null);
            let out = json!({"output_type": "stream", "name": "stdout", "text": "2"});
            schema::set_outputs(&mut txn, &a, vec![out]);
        }
        let update = client.transact().encode_state_as_update_v1(&before);
        room.receive(1, &Message::Sync(SyncMessage::Update(update)).encode_v1())
            .unwrap();
        let a = cell_json(&mut room, "a");
        assert_eq!(a["metadata"]["ganymede"], json!({"keep": 1}));
        assert_eq!(a["outputs"][0]["text"], json!("2"));

        let recorded = room_run(&mut room, &mut x, "78");
        assert_eq!(recorded, json!({"keep": 1, "ran_source_hash": "78"}));
    }

    #[test]
    fn recording_the_hash_keeps_the_clean_run_stamp() {
        let nb: ganymede_nbformat::Notebook = serde_json::from_value(json!({
            "cells": [{"cell_type": "code", "id": "a", "metadata": {},
                       "source": "1", "outputs": [], "execution_count": 1}],
            "metadata": {"ganymede": {"clean_run": {"at": "t", "by": "b", "kernel": "k"}}},
            "nbformat": 4, "nbformat_minor": 5
        }))
        .unwrap();
        let mut room = Room::notebook_room(&nb, "a.ipynb");
        let mut x = exec();
        run(&mut x, &mut room, &[id("a")], "A");
        // The same count a clean run left.
        x.finish(
            &mut room,
            "a",
            Some(Vec::new()),
            Some(1),
            Some("5".into()),
            false,
        );
        let stamp = room.edit(|txn, roots| schema::clean_run(txn, roots)).0;
        assert_eq!(stamp, Some(json!({"at": "t", "by": "b", "kernel": "k"})));
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
