//! `POST /api/notebooks/run` (#652): the run contract over HTTP, beside
//! `ganymede execute -p --results` (#650) and the host API's
//! `notebooks.run` (`docs/host-api.md`). A Ganymede extension;
//! jupyter_server has no such route.
//!
//! ```text
//! {"path": "reports/daily.ipynb", "params": {"day": "2026-11-10"}, "timeout": 600}
//! ```
//!
//! Runs the notebook top to bottom in a fresh kernel from its own
//! kernelspec, with `params` in `GANYMEDE_PARAMS` (`{}` without), and
//! answers when it ends with what one line of `--results` holds:
//!
//! ```text
//! {"path": "...", "status": "ok" | "error", "results": {name: value}, "error"?: "..."}
//! ```
//!
//! The notebook is read from its room when one is open, so unsaved edits
//! run, and nothing is written: not the notebook, not its room. The run's
//! kernel is listed in `/api/kernels` while it runs and shut down when the
//! run ends, times out, or the client goes away. `400` for a bad request
//! (an uninstalled kernelspec among them) and `404` for a notebook that
//! can't be read are answered before a
//! kernel starts; once one has, a failure is `200` with `status: "error"`
//! and the results the cells published before it.

use crate::routes::{HttpError, LenientJson};
use crate::AppState;
use axum::extract::State;
use axum::Json;
use ganymede_api::KernelManager;
use ganymede_exec::{Event, Options};
use ganymede_nbformat::{named_results_of, params_env, Cell, PARAMS_ENV};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// The longest run, and the run's limit when the request names none.
const MAX_RUN: Duration = Duration::from_secs(3600);

#[derive(Deserialize, Default)]
pub struct RunBody {
    path: Option<String>,
    params: Option<Value>,
    /// Seconds for the whole run.
    timeout: Option<f64>,
    /// Only `false` for now: the run contract's copies (#652) come later.
    save: Option<Value>,
}

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError(400, msg.into())
}

/// Shuts the run's kernel down when dropped: when the run ends, and when
/// the client goes away and the handler's future is dropped mid-run.
struct KernelGuard {
    kernels: Arc<KernelManager>,
    id: String,
}

impl Drop for KernelGuard {
    fn drop(&mut self) {
        let (kernels, id) = (self.kernels.clone(), std::mem::take(&mut self.id));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = kernels.shutdown(&id).await;
            });
        }
    }
}

pub async fn run(
    State(state): State<AppState>,
    LenientJson(body): LenientJson<RunBody>,
) -> Result<Json<Value>, HttpError> {
    let path = body
        .path
        .map(|p| p.trim_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| bad("path is required"))?;
    let params = match body.params {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(other) => return Err(bad(format!("params must be a JSON object, got {other}"))),
    };
    let env = params_env(&params).map_err(bad)?;
    let limit = match body.timeout {
        None => MAX_RUN,
        // Clamped first: `from_secs_f64` panics past `Duration::MAX`.
        Some(t) if t.is_finite() && t > 0.0 => {
            Duration::from_secs_f64(t.min(MAX_RUN.as_secs_f64()))
        }
        Some(t) => {
            return Err(bad(format!(
                "timeout must be a positive number of seconds, got {t}"
            )))
        }
    };
    match body.save {
        None | Some(Value::Bool(false)) | Some(Value::Null) => {}
        Some(other) => {
            return Err(bad(format!(
                "save must be false; this server keeps no copies of runs yet, got {other}"
            )))
        }
    }

    let mut nb = crate::mcp::notebook_for_search(&state, &path)
        .await
        .ok_or_else(|| HttpError(404, format!("cannot read {path} as a notebook")))?;
    let kernel_name = nb
        .metadata
        .get("kernelspec")
        .and_then(|k| k.get("name"))
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad(format!("{path} has no metadata.kernelspec.name")))?;

    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let model = state
        .kernels
        .start_with_env(
            Some(&kernel_name),
            Some(&path),
            Some(state.kernels.session_name(&path, &name)),
            vec![(PARAMS_ENV.to_string(), env)],
        )
        .await
        .map_err(|e| match e.status {
            // A kernelspec that isn't installed: the request's fault, and a
            // 404 would read as a missing notebook.
            404 => bad(format!(
                "{path} needs the kernel {kernel_name}, which isn't installed"
            )),
            _ => HttpError::from(e),
        })?;
    let _guard = KernelGuard {
        kernels: state.kernels.clone(),
        id: model.id.clone(),
    };

    let opts = Options {
        // As `ganymede execute`: a cell that asks for input fails.
        allow_stdin: false,
        ..Default::default()
    };
    // Cells this run started: only their outputs are its results. A cell
    // after a failure keeps what an earlier run saved.
    let mut ran = BTreeSet::new();
    let outcome = tokio::time::timeout(limit, async {
        let kernel = state.kernels.wait_started(&model.id).await;
        if let Err(e) = kernel {
            return Err(format!(
                "the kernel {kernel_name} failed to start: {}",
                e.message
            ));
        }
        let session = state
            .kernels
            .get(&model.id)
            .map_err(|e| e.message)?
            .session();
        let mut on_event = |e: Event<'_>| {
            if let Event::CellStarted { index, .. } = e {
                ran.insert(index);
            }
        };
        ganymede_exec::execute_notebook_with(&session, &mut nb, &opts, &mut on_event)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "the run didn't finish in {} seconds",
            limit.as_secs_f64()
        ))
    });

    let results = named_results_of(ran.iter().flat_map(|&i| match nb.cells.get(i) {
        Some(Cell::Code(c)) => c.outputs.as_slice(),
        _ => &[],
    }));
    let mut answer = json!({
        "path": path,
        "status": if outcome.is_ok() { "ok" } else { "error" },
        "results": results,
    });
    if let Err(e) = outcome {
        answer["error"] = e.into();
    }
    Ok(Json(answer))
}
