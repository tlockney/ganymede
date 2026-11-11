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
//! run, and neither the notebook nor its room is written. `"save":
//! "copy"` (#658) keeps the executed notebook under [`RUNS_DIR`] and adds
//! its path to the answer as `copy`. The run's
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

/// Where `save: "copy"` writes runs, under the root: `runs/<notebook's
/// path without .ipynb>/<UTC start time>.ipynb`.
pub const RUNS_DIR: &str = "runs";

#[derive(Deserialize, Default)]
pub struct RunBody {
    path: Option<String>,
    params: Option<Value>,
    /// Seconds for the whole run.
    timeout: Option<f64>,
    /// `false` (the default) writes nothing; `"copy"` writes the executed
    /// notebook under [`RUNS_DIR`] and answers its path as `copy`.
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
            if id.is_empty() {
                return;
            }
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
    let request = RunRequest::parse(body).map_err(bad)?;
    run_notebook(&state, &request, Vec::new())
        .await
        .map(Json)
        .map_err(|e| HttpError(e.status, e.message))
}

/// A run, checked: what `POST /api/notebooks/run` and the host API's
/// `notebooks.run` both take.
pub struct RunRequest {
    pub path: String,
    pub params: Map<String, Value>,
    pub limit: Duration,
    /// Write the executed notebook as a copy (`save: "copy"`).
    pub save_copy: bool,
}

impl RunRequest {
    /// `{ path, params?, timeout?, save? }`, with the HTTP body's checks.
    pub fn parse(body: RunBody) -> Result<RunRequest, String> {
        let path = body
            .path
            .map(|p| p.trim_matches('/').to_string())
            .filter(|p| !p.is_empty())
            .ok_or("path is required")?;
        let params = match body.params {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m,
            Some(other) => return Err(format!("params must be a JSON object, got {other}")),
        };
        let limit = match body.timeout {
            None => MAX_RUN,
            // Clamped first: `from_secs_f64` panics past `Duration::MAX`.
            Some(t) if t.is_finite() && t > 0.0 => {
                Duration::from_secs_f64(t.min(MAX_RUN.as_secs_f64()))
            }
            Some(t) => {
                return Err(format!(
                    "timeout must be a positive number of seconds, got {t}"
                ))
            }
        };
        let save_copy = match body.save {
            None | Some(Value::Bool(false)) | Some(Value::Null) => false,
            Some(Value::String(s)) if s == "copy" => true,
            Some(other) => return Err(format!("save must be false or \"copy\", got {other}")),
        };
        Ok(RunRequest {
            path,
            params,
            limit,
            save_copy,
        })
    }

    /// The same checks on a JSON value, as the host API gets it.
    pub fn from_value(v: Value) -> Result<RunRequest, String> {
        let body: RunBody =
            serde_json::from_value(v).map_err(|e| format!("bad notebooks.run params: {e}"))?;
        Self::parse(body)
    }
}

/// Why a run didn't start: an HTTP status and a message.
#[derive(Debug)]
pub struct RunError {
    pub status: u16,
    pub message: String,
}

/// Run `request` in a fresh kernel, with `env` added to the kernel's
/// launch variables beside `GANYMEDE_PARAMS`, and answer with what one
/// line of `--results` holds. An error is a run that never started.
pub async fn run_notebook(
    state: &AppState,
    request: &RunRequest,
    mut env: Vec<(String, String)>,
) -> Result<Value, RunError> {
    let path = &request.path;
    let params = params_env(&request.params).map_err(|e| RunError {
        status: 400,
        message: e,
    })?;
    env.insert(0, (PARAMS_ENV.to_string(), params));
    let limit = request.limit;
    let started = chrono::Utc::now();
    let mut nb = crate::mcp::notebook_for_search(state, path)
        .await
        .ok_or_else(|| RunError {
            status: 404,
            message: format!("cannot read {path} as a notebook"),
        })?;
    let kernel_name = nb
        .metadata
        .get("kernelspec")
        .and_then(|k| k.get("name"))
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RunError {
            status: 400,
            message: format!("{path} has no metadata.kernelspec.name"),
        })?;

    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let model = state
        .kernels
        .start_with_env(
            Some(&kernel_name),
            Some(path),
            Some(state.kernels.session_name(path, &name)),
            env,
        )
        .await
        .map_err(|e| match e.status {
            // A kernelspec that isn't installed: the request's fault, and a
            // 404 would read as a missing notebook.
            404 => RunError {
                status: 400,
                message: format!("{path} needs the kernel {kernel_name}, which isn't installed"),
            },
            status => RunError {
                status,
                message: e.message,
            },
        })?;
    let mut guard = KernelGuard {
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

    // Shut the kernel down before answering, so a caller that counts
    // kernels, or waits for a slot, sees it gone. The guard covers a call
    // dropped before this point.
    let _ = state.kernels.shutdown(&guard.id).await;
    guard.id.clear();

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
    // A failed run is kept too: its copy shows where it stopped.
    if request.save_copy {
        // As `ganymede execute` records them: the copy says what it ran with.
        ganymede_nbformat::record_params(
            &mut nb,
            (!request.params.is_empty()).then_some(&request.params),
        );
        match save_copy(state, path, &nb, started).await {
            Ok(copy) => answer["copy"] = copy.into(),
            Err(e) => {
                answer["status"] = "error".into();
                let before = answer["error"].as_str().map(|b| format!("{b}; "));
                answer["error"] =
                    format!("{}the copy wasn't saved: {e}", before.unwrap_or_default()).into();
            }
        }
    }
    Ok(answer)
}

/// The API path of `path`'s copy for a run started at `started`, `n`
/// for the nth of that second.
fn copy_path(path: &str, started: chrono::DateTime<chrono::Utc>, n: u32) -> String {
    let stem = path.strip_suffix(".ipynb").unwrap_or(path);
    let stamp = started.format("%Y-%m-%dT%H%M%SZ");
    match n {
        0 | 1 => format!("{RUNS_DIR}/{stem}/{stamp}.ipynb"),
        n => format!("{RUNS_DIR}/{stem}/{stamp}-{n}.ipynb"),
    }
}

/// Write the executed notebook as `path`'s copy for this run, and return
/// the copy's API path. The source notebook and its room are untouched.
async fn save_copy(
    state: &AppState,
    path: &str,
    nb: &ganymede_nbformat::Notebook,
    started: chrono::DateTime<chrono::Utc>,
) -> Result<String, String> {
    use ganymede_contents::{ContentType, Format, GetOptions, SaveRequest};
    let first = copy_path(path, started, 0);
    let dir = first.rsplit_once('/').map(|(d, _)| d).unwrap_or_default();
    let mut at = String::new();
    for part in dir.split('/') {
        if !at.is_empty() {
            at.push('/');
        }
        at.push_str(part);
        let request = SaveRequest {
            kind: ContentType::Directory,
            format: None,
            content: None,
            last_modified: None,
            chunk: None,
            if_match: None,
        };
        state
            .contents
            .save(&at, request)
            .await
            .map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_value(nb).map_err(|e| e.to_string())?;
    // One copy at a time, so two runs started in the same second can't
    // both find the same name free.
    static PICKING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _picking = PICKING.lock().await;
    for n in 1..=100 {
        let copy = copy_path(path, started, n);
        let exists = state
            .contents
            .get(
                &copy,
                GetOptions {
                    content: false,
                    ..Default::default()
                },
            )
            .await
            .is_ok();
        if exists {
            continue;
        }
        let request = SaveRequest {
            kind: ContentType::Notebook,
            format: Some(Format::Json),
            content: Some(content),
            last_modified: None,
            chunk: None,
            if_match: None,
        };
        state
            .contents
            .save(&copy, request)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(copy);
    }
    Err(format!("too many runs of {path} in one second"))
}

#[cfg(test)]
mod tests {
    use super::copy_path;

    #[test]
    fn copies_are_named_for_the_notebook_and_the_start() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-11T19:30:05.250Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            copy_path("reports/daily.ipynb", at, 1),
            "runs/reports/daily/2026-11-11T193005Z.ipynb"
        );
        assert_eq!(
            copy_path("reports/daily.ipynb", at, 0),
            copy_path("reports/daily.ipynb", at, 1)
        );
        assert_eq!(
            copy_path("reports/daily.ipynb", at, 2),
            "runs/reports/daily/2026-11-11T193005Z-2.ipynb"
        );
        assert_eq!(
            copy_path("index.ipynb", at, 3),
            "runs/index/2026-11-11T193005Z-3.ipynb"
        );
    }
}
