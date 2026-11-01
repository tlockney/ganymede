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
        cells: Vec<(String, String)>,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    /// The worker wants a cell as it is now, to run it.
    Get(String, oneshot::Sender<Option<Cell>>),
    /// New outputs for a cell.
    Outputs(String, Vec<Value>),
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
                    });
                }
                let _ = reply.send(checked);
                fx
            }
            RoomCmd::Get(id, reply) => {
                let cell = room
                    .notebook()
                    .and_then(Result::ok)
                    .and_then(|nb| nb.cells.into_iter().find(|c| c.id() == Some(id.as_str())));
                let _ = reply.send(cell);
                Effects::default()
            }
            RoomCmd::Outputs(id, outputs) => {
                room.edit(|txn, roots| {
                    if let Some(cell) = schema::find_cell(txn, roots, &id) {
                        schema::set_outputs(txn, &cell, outputs);
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
fn enqueue(room: &mut Room, cells: &[(String, String)]) -> (Result<(), Refused>, Effects) {
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
            if ganymede_documents::murmur2(source.as_bytes(), 0).to_string() != *hash {
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
    ids: Vec<String>,
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
        self.cells.push(Cell::Code(cell));
        self.ids.push(cell_id.clone());
        let index = self.cells.len() - 1;
        kernel.touch();

        let mut count: Option<i64> = None;
        let ids = self.ids.clone();
        let mut handler = |event: Event<'_>| {
            if let Event::Output {
                msg,
                changed,
                cells,
                ..
            } = event
            {
                if msg.msg_type() == "execute_input" {
                    count = msg.content["execution_count"].as_i64();
                }
                for &i in changed {
                    if let (Some(Cell::Code(c)), Some(id)) = (cells.get(i), ids.get(i)) {
                        let _ = room.send(Cmd::Exec(RoomCmd::Outputs(
                            id.clone(),
                            live_outputs(&c.outputs),
                        )));
                    }
                }
            }
        };
        let status = self
            .run
            .execute_cell_with(&kernel.session(), &mut self.cells, index, &mut handler)
            .await;
        kernel.touch();
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
    fn channel(&self, id: &str) -> watch::Receiver<bool> {
        let mut q = self.queued.lock().expect("requests");
        // Forget ids nobody waited for.
        q.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(60));
        q.entry(id.to_string())
            .or_insert_with(|| (watch::channel(false).0, Instant::now()))
            .0
            .subscribe()
    }

    fn mark(&self, id: &str) {
        let mut q = self.queued.lock().expect("requests");
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
        cells.push((id.to_string(), hash));
    }
    // 404 for an unknown kernel, as the other kernel endpoints answer.
    state.kernels.get(&kernel_id).map_err(HttpError::from)?;

    if let Some(prev) = body
        .previous_request_id
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        let mut rx = state.collab.requests.channel(prev);
        let waited = tokio::time::timeout(PREDECESSOR_TIMEOUT, rx.wait_for(|q| *q)).await;
        if !matches!(waited, Ok(Ok(_))) {
            return Err(HttpError(
                408,
                "Timed out waiting for previous_request_id to be enqueued".into(),
            ));
        }
    }

    let (reply, answer) = oneshot::channel();
    let cmd = Cmd::Exec(RoomCmd::Enqueue {
        kernels: state.kernels.clone(),
        kernel_id,
        cells,
        reply,
    });
    send_to_notebook_room(&state.collab, &state.contents, &document_id, cmd)
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
