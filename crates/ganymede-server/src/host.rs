//! The server's answers to the host API (`docs/host-api.md`): what cell
//! code may ask the server through the comm `ganymede-host` opens to each
//! kernel. The calls share their code with the MCP tools, and return data
//! where the tools return text for a model.
//!
//! Calls:
//! - `notebooks.list { dir? }`: notebooks and directories in `dir`.
//! - `notebooks.read { path }`: `{ notebook, hashes }`, the notebook as
//!   nbformat JSON (from its room when one is open) and each cell's
//!   source hash (MurmurHash2, seed 0) by cell id.
//! - `notebooks.search { query, dir?, outputs?, cell_type?, limit? }`:
//!   `notebooks_search`'s matches.
//! - `notebooks.results { path }`: `{ name: value }`, the named results in
//!   the notebook's outputs (the copy `notebooks.read` returns).
//! - `notebooks.run { path, params?, timeout?, save? }`: runs another
//!   notebook in a fresh kernel (`POST /api/notebooks/run`'s code) and
//!   returns `{ path, status, results, error?, copy? }`, under the `[host] run`
//!   grant. The run's kernel gets the intersection of the caller's grant
//!   and the notebook's, runs nest at most 3 deep, and each kernel has
//!   at most 2 runs in flight.
//! - `self.add { cell_type, source, after? }`, `self.edit { id, source,
//!   source_hash }`, `self.delete { id, source_hash }`: the kernel's own
//!   notebook, through its room, so every client sees the change. An
//!   edit or delete names the source hash it read (`notebooks.read`), and
//!   fails `source_mismatch` when the cell changed since.
//!
//! Reads reach the paths in the `[host]` `read` grant (the whole root by
//! default), matched by whole path segments against where a path leads
//! once symlinks are resolved; a notebook may always read itself. A
//! kernel's notebook is the one of the session the server started it for.

use crate::collab::{hold_notebook, notebook_room_id, with_notebook, Hold};
use crate::mcp::{notebook_for_search, search, search_notebooks};
use crate::room_ops;
use crate::routes::{HttpError, LenientJson};
use crate::AppState;
use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::Json;
use ganymede_contents::{ContentType, GetOptions};
use ganymede_documents::schema;
use ganymede_host::{Caller, Hello, HostError, Responder};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// The `[host]` table of the config file.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
    /// API paths the `notebooks.*` reads may reach; `""` is the root.
    pub read: Vec<String>,
    /// Whether a notebook may add, edit and delete its own cells
    /// (`self.*`).
    pub edit_self: bool,
    /// Notebooks a notebook may run (`notebooks.run`); `[]` is none, `""`
    /// is all.
    pub run: Vec<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            read: vec![String::new()],
            edit_self: true,
            run: Vec::new(),
        }
    }
}

/// A notebook's own grant, kept by file id beside the file id database
/// (`ganymede grant`). A key that is set replaces the `[host]` default
/// for that notebook; one that isn't keeps the default.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_self: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<Vec<String>>,
}

impl Grant {
    pub fn is_empty(&self) -> bool {
        self == &Grant::default()
    }
}

/// Who is calling: the kernel's notebook and what it may do.
#[derive(Clone)]
struct Who {
    notebook: Option<String>,
    read: Vec<String>,
    edit_self: bool,
    run: Vec<String>,
    /// How many `notebooks.run` calls deep this kernel is: 0 for a
    /// notebook's own kernel.
    depth: u32,
    /// For a run, the read and run grants of every kernel up the chain
    /// that started it. A path must pass each of them as well, checked
    /// with symlinks resolved, so a grant naming a symlink can't widen
    /// what the caller may do.
    outer_read: Vec<Vec<String>>,
    outer_run: Vec<Vec<String>>,
}

impl Who {
    /// A caller with no grant at all: no notebook, nothing to read or run.
    fn nothing() -> Self {
        Who {
            notebook: None,
            read: Vec::new(),
            edit_self: false,
            run: Vec::new(),
            depth: MAX_DEPTH,
            outer_read: Vec::new(),
            outer_run: Vec::new(),
        }
    }

    fn notebook(&self) -> Option<&str> {
        self.notebook.as_deref()
    }

    /// Whether the grant covers the whole root, so no path needs checking.
    fn reads_everything(&self) -> bool {
        self.read.iter().any(|g| g.is_empty())
    }
}

const READS: [&str; 4] = [
    "notebooks.list",
    "notebooks.read",
    "notebooks.search",
    "notebooks.results",
];
const SELF_EDITS: [&str; 3] = ["self.add", "self.edit", "self.delete"];

/// The launch variable that names a `notebooks.run` kernel's run. The
/// server reads its own record of the kernel's launch variables, so the
/// kernel can't claim another run's grant.
const RUN_VAR: &str = "GANYMEDE_HOST_RUN";
/// Runs started from runs nest at most this deep.
const MAX_DEPTH: u32 = 3;
/// Runs in flight per calling kernel; more wait their turn.
const WIDTH: usize = 2;

/// The paths covered by both `a` and `b`, as grant lists: a path of one
/// that lies under a path of the other.
fn intersect(a: &[String], b: &[String]) -> Vec<String> {
    let under = |p: &str, list: &[String]| {
        list.iter()
            .any(|g| ganymede_contents::path_under(p, g).is_some())
    };
    let mut out: Vec<String> = a
        .iter()
        .filter(|p| under(p, b))
        .chain(b.iter().filter(|p| under(p, a)))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Forgets a run's grant when its call ends, however it ends.
struct RunEntry<'a> {
    runs: &'a std::sync::Mutex<std::collections::HashMap<String, Who>>,
    token: String,
}

impl Drop for RunEntry<'_> {
    fn drop(&mut self) {
        self.runs.lock().unwrap().remove(&self.token);
    }
}

pub struct HostApi {
    state: AppState,
    read: Vec<String>,
    edit_self: bool,
    run: Vec<String>,
    /// The grants of runs in flight, by their `GANYMEDE_HOST_RUN` token.
    runs: std::sync::Mutex<std::collections::HashMap<String, Who>>,
    /// Each calling kernel's run slots.
    slots: std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Semaphore>>>,
    /// Each kernel's notebook session, as first seen: `hello` at kernel
    /// start sees the session the kernel was started for, and a notebook
    /// that joins the kernel later doesn't take its place.
    sessions: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// Kernels the server didn't start, with their notebooks:
    /// `ganymede execute`'s ([`HostApi::pin`]).
    pinned: std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,
    /// The notebook room each kernel's `self.*` calls hold open, by kernel
    /// id, with the room's id ([`HostApi::hold`]). Shared with the task
    /// that lets a server kernel's go when it ends.
    holds: Holds,
}

impl HostApi {
    pub fn new(state: AppState, config: &HostConfig) -> Self {
        HostApi {
            state,
            read: config
                .read
                .iter()
                .map(|p| ganymede_contents::normalize_api_path(p))
                .collect(),
            edit_self: config.edit_self,
            run: config
                .run
                .iter()
                .map(|p| ganymede_contents::normalize_api_path(p))
                .collect(),
            runs: Default::default(),
            slots: Default::default(),
            sessions: Default::default(),
            pinned: Default::default(),
            holds: Default::default(),
        }
    }

    /// Answer for `kernel_id`, a kernel the server didn't start, as the
    /// kernel of `notebook` (an API path): `ganymede execute` pins each
    /// kernel it runs a notebook on. [`HostApi::unpin`] forgets it.
    pub fn pin(&self, kernel_id: &str, notebook: Option<String>) {
        self.pinned.lock().unwrap().insert(
            kernel_id.to_string(),
            notebook.map(|p| ganymede_contents::normalize_api_path(&p)),
        );
    }

    pub fn unpin(&self, kernel_id: &str) {
        self.pinned.lock().unwrap().remove(kernel_id);
        self.slots.lock().unwrap().remove(kernel_id);
        self.holds.lock().unwrap().remove(kernel_id);
    }

    /// Whether `kernel_id` is running: one of the server's, or pinned.
    fn is_live(&self, kernel_id: &str) -> bool {
        self.state.kernels.get(kernel_id).is_ok()
            || self.pinned.lock().unwrap().contains_key(kernel_id)
    }

    /// The API path of the notebook `kernel_id` was started for: its
    /// first notebook session's.
    fn notebook(&self, kernel_id: &str) -> Option<String> {
        if let Some(notebook) = self.pinned.lock().unwrap().get(kernel_id) {
            return notebook.clone();
        }
        let sessions = &self.state.sessions;
        let mut seen = self.sessions.lock().unwrap();
        // The session it was started for, renamed or not, while it lasts.
        let path = seen
            .get(kernel_id)
            .and_then(|id| sessions.notebook_path(id))
            .or_else(|| {
                // Forget kernels that are gone, so the map stays as small
                // as the set of running kernels.
                seen.retain(|k, _| self.state.kernels.get(k).is_ok());
                let (id, path) = sessions.notebook_of_kernel(kernel_id)?;
                seen.insert(kernel_id.to_string(), id);
                Some(path)
            })?;
        Some(ganymede_contents::normalize_api_path(&path))
    }

    /// The caller's notebook and its grant. A `notebooks.run` kernel has
    /// the grant its run was given; any other has its notebook's.
    async fn who(&self, caller: &Caller) -> Who {
        let kernel = self.state.kernels.get(&caller.kernel_id).ok();
        let token = kernel
            .as_ref()
            .and_then(|k| k.launch_var(RUN_VAR).map(str::to_string));
        // A `POST /api/notebooks/run` kernel has no session: it answers as
        // the notebook it runs, with that notebook's grant (#745). It
        // writes at most a copy, never its notebook, so no self.* calls.
        let run_of = kernel
            .as_ref()
            .and_then(|k| k.launch_var(crate::run::NOTEBOOK_ENV).map(str::to_string));
        if let Some(token) = token {
            // A run whose call has ended has no grant left.
            return self
                .runs
                .lock()
                .unwrap()
                .get(&token)
                .cloned()
                .unwrap_or_else(Who::nothing);
        }
        if let Some(id) = run_of {
            // Where the notebook is now; a notebook that is gone leaves no
            // grant, never the defaults.
            let Some((path, own)) = self.state.collab.path_and_grant(&id).await else {
                return Who::nothing();
            };
            let mut who = self.grant_from(Some(path), own);
            who.edit_self = false;
            return who;
        }
        let mut who = self.grant_for(self.notebook(&caller.kernel_id)).await;
        // A pinned kernel's notebook is written by whoever pinned it (`ganymede
        // execute` writes it when the run ends), so it has no self.* calls,
        // whatever its grant says.
        if self.pinned.lock().unwrap().contains_key(&caller.kernel_id) {
            who.edit_self = false;
        }
        who
    }

    /// `notebook`'s grant: the `[host]` defaults, with the notebook's own
    /// grant in place of any key it sets.
    async fn grant_for(&self, notebook: Option<String>) -> Who {
        let own = match &notebook {
            Some(path) => self.state.collab.grant(path).await,
            None => None,
        };
        self.grant_from(notebook, own)
    }

    /// `notebook`'s grant from `own`, its stored grant (JSON), as
    /// [`HostApi::grant_for`].
    fn grant_from(&self, notebook: Option<String>, own: Option<String>) -> Who {
        let own: Grant = own
            .and_then(|g| {
                serde_json::from_str(&g)
                    .map_err(|e| tracing::warn!("grant for {notebook:?}: {e}"))
                    .ok()
            })
            .unwrap_or_default();
        Who {
            read: match own.read {
                Some(read) => read
                    .iter()
                    .map(|p| ganymede_contents::normalize_api_path(p))
                    .collect(),
                None => self.read.clone(),
            },
            edit_self: own.edit_self.unwrap_or(self.edit_self),
            run: match own.run {
                Some(run) => run
                    .iter()
                    .map(|p| ganymede_contents::normalize_api_path(p))
                    .collect(),
                None => self.run.clone(),
            },
            depth: 0,
            outer_read: Vec::new(),
            outer_run: Vec::new(),
            notebook,
        }
    }

    /// Where `path` leads once symlinks are resolved, as an API path; the
    /// path itself when it doesn't exist (or the root is unknown), and
    /// `None` when it leads outside the root.
    fn resolved(&self, path: &str) -> Option<String> {
        let Some(root) = self.state.collab.root() else {
            return Some(path.to_string());
        };
        let Ok(root) = std::fs::canonicalize(root) else {
            return Some(path.to_string());
        };
        let Ok(real) = std::fs::canonicalize(root.join(path)) else {
            return Some(path.to_string());
        };
        let rel = real.strip_prefix(&root).ok()?;
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect::<Option<_>>()?;
        Some(parts.join("/"))
    }

    /// Whether `caller` may read `path`, a normalized API path: where it
    /// leads must be in the grant, so a symlink in a granted folder
    /// doesn't open another.
    fn may_read(&self, who: &Who, path: &str) -> bool {
        if who.notebook() == Some(path) || who.reads_everything() {
            return true;
        }
        self.covers(&who.read, path) && who.outer_read.iter().all(|g| self.covers(g, path))
    }

    /// Whether `who` may run `path`, a normalized API path.
    fn may_run(&self, who: &Who, path: &str) -> bool {
        self.covers(&who.run, path) && who.outer_run.iter().all(|g| self.covers(g, path))
    }

    /// Whether `grants` cover `path`, a normalized API path, where it
    /// leads once symlinks are resolved.
    fn covers(&self, grants: &[String], path: &str) -> bool {
        if grants.iter().any(|g| g.is_empty()) {
            return true;
        }
        let Some(real) = self.resolved(path) else {
            return false;
        };
        // Grants resolved the same way, so a granted folder that is itself
        // a symlink still covers what is in it.
        grants.iter().any(|g| {
            ganymede_contents::path_under(&real, g).is_some()
                || self
                    .resolved(g)
                    .is_some_and(|g| ganymede_contents::path_under(&real, &g).is_some())
        })
    }

    /// Whether a directory may be listed or walked: it is readable, or a
    /// grant or the notebook itself lies under it.
    fn may_enter(&self, who: &Who, dir: &str) -> bool {
        self.may_read(who, dir)
            || who
                .notebook()
                .is_some_and(|n| ganymede_contents::path_under(n, dir).is_some())
            || who
                .read
                .iter()
                .any(|g| ganymede_contents::path_under(g, dir).is_some())
    }

    fn checked_path(&self, who: &Who, params: &Value, key: &str) -> Result<String, HostError> {
        let raw = match params.get(key) {
            None | Some(Value::Null) if key == "dir" => "",
            Some(Value::String(s)) => s.as_str(),
            _ => return Err(HostError::failed(format!("`{key}` must be a string"))),
        };
        if raw.split('/').any(|c| c == "..") {
            return Err(HostError::denied(format!("{raw}: `..` is not allowed")));
        }
        let path = ganymede_contents::normalize_api_path(raw);
        // A directory to search or list is allowed when anything under it
        // is: the walk then filters what it reads.
        let allowed = if key == "dir" {
            self.may_enter(who, &path)
        } else {
            self.may_read(who, &path)
        };
        if !allowed {
            return Err(HostError::denied(format!(
                "{} is outside this notebook's read grant",
                if path.is_empty() { "the root" } else { &path }
            )));
        }
        Ok(path)
    }

    async fn list(&self, who: &Who, params: &Value) -> Result<Value, HostError> {
        let dir = self.checked_path(who, params, "dir")?;
        let model = self
            .state
            .contents
            .get(
                &dir,
                GetOptions {
                    content: true,
                    type_hint: Some(ContentType::Directory),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| HostError::failed(e.to_string()))?;
        let mut out = Vec::new();
        if let Some(Value::Array(items)) = model.content {
            for item in items {
                let path = item["path"].as_str().unwrap_or_default().to_string();
                let kind = item["type"].as_str().unwrap_or_default();
                let modified = item["last_modified"].clone();
                match kind {
                    "directory"
                        if item["name"] != ".ipynb_checkpoints" && self.may_enter(who, &path) =>
                    {
                        out.push(json!({"path": path, "type": "directory", "modified": modified}));
                    }
                    "notebook" if self.may_read(who, &path) => {
                        let nb = notebook_for_search(&self.state, &path).await;
                        let title = nb.as_ref().and_then(search::title);
                        let kernel = nb.as_ref().and_then(|n| {
                            n.metadata
                                .get("kernelspec")?
                                .get("name")?
                                .as_str()
                                .map(str::to_string)
                        });
                        out.push(json!({"path": path, "type": "notebook", "title": title,
                                        "kernel": kernel, "modified": modified}));
                    }
                    _ => {}
                }
            }
        }
        Ok(Value::Array(out))
    }

    /// The notebook at `params[key]` as `notebooks.read` sees it: from its
    /// room when one is open, otherwise from disk.
    async fn load(
        &self,
        who: &Who,
        params: &Value,
    ) -> Result<ganymede_nbformat::Notebook, HostError> {
        let path = self.checked_path(who, params, "path")?;
        notebook_for_search(&self.state, &path)
            .await
            .ok_or_else(|| HostError::failed(format!("cannot read {path} as a notebook")))
    }

    async fn results(&self, who: &Who, params: &Value) -> Result<Value, HostError> {
        let nb = self.load(who, params).await?;
        Ok(Value::Object(ganymede_nbformat::named_results(&nb)))
    }

    async fn read(&self, who: &Who, params: &Value) -> Result<Value, HostError> {
        let nb = self.load(who, params).await?;
        let hashes: serde_json::Map<String, Value> = nb
            .cells
            .iter()
            .filter_map(|c| {
                let id = c.id()?.to_string();
                let source = c
                    .source()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();
                let hash = ganymede_documents::murmur2(source.as_bytes(), 0);
                Some((id, Value::String(hash.to_string())))
            })
            .collect();
        let notebook = serde_json::to_value(&nb).map_err(|e| HostError::failed(e.to_string()))?;
        Ok(json!({"notebook": notebook, "hashes": hashes}))
    }

    /// Whether `who` may start a run of `path` now, keeping a copy if
    /// `save_copy`.
    fn check_run(&self, who: &Who, path: &str, save_copy: bool) -> Result<(), HostError> {
        if !self.may_run(who, path) {
            return Err(HostError::denied(format!(
                "{path} is outside this notebook's run grant (`[host] run`, or `ganymede grant NOTEBOOK run=PATH`)"
            )));
        }
        // A copy holds the notebook's source and every output. A run grant
        // alone gives only named results, so keeping a copy, which the
        // caller might read under runs/, needs a read of the source too.
        // Every outer caller too: a run may read its own notebook, but a
        // copy of it under runs/ is for whoever may read runs/.
        if save_copy
            && !(self.may_read(who, path) && who.outer_read.iter().all(|g| self.covers(g, path)))
        {
            return Err(HostError::denied(format!(
                "save: \"copy\" needs {path} inside this notebook's read grant as well as its run grant"
            )));
        }
        if who.depth >= MAX_DEPTH {
            return Err(HostError::denied(format!(
                "runs nest at most {MAX_DEPTH} deep"
            )));
        }
        Ok(())
    }

    async fn run(&self, caller: &Caller, who: &Who, params: Value) -> Result<Value, HostError> {
        let mut request = crate::run::RunRequest::from_value(params).map_err(HostError::failed)?;
        if request.path.split('/').any(|c| c == "..") {
            return Err(HostError::denied(format!(
                "{}: `..` is not allowed",
                request.path
            )));
        }
        let path = ganymede_contents::normalize_api_path(&request.path);
        self.check_run(who, &path, request.save_copy)?;
        let slot = {
            let mut slots = self.slots.lock().unwrap();
            slots.retain(|k, _| self.is_live(k));
            slots
                .entry(caller.kernel_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(WIDTH)))
                .clone()
        };
        let _permit = slot
            .acquire_owned()
            .await
            .map_err(|e| HostError::failed(e.to_string()))?;
        // Check again, with the grant as it is now, once a slot is free: a
        // grant changed while the call waited applies to it. The run then
        // keeps the grant it starts with.
        let who = &self.who(caller).await;
        self.check_run(who, &path, request.save_copy)?;
        // Grants narrow, never widen: the run gets what both the caller
        // and the notebook it runs may do. A run never writes its own
        // notebook (at most a copy under runs/), so it has no self.* calls.
        let callee = self.grant_for(Some(path.clone())).await;
        let grant = Who {
            read: intersect(&who.read, &callee.read),
            run: intersect(&who.run, &callee.run),
            edit_self: false,
            depth: who.depth + 1,
            notebook: Some(path.clone()),
            outer_read: [who.outer_read.clone(), vec![who.read.clone()]].concat(),
            outer_run: [who.outer_run.clone(), vec![who.run.clone()]].concat(),
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.runs.lock().unwrap().insert(token.clone(), grant);
        let _entry = RunEntry {
            runs: &self.runs,
            token: token.clone(),
        };
        request.path = path;
        crate::run::run_notebook(&self.state, &request, vec![(RUN_VAR.to_string(), token)])
            .await
            .map_err(|e| HostError::failed(e.message))
    }

    async fn search(&self, who: &Who, params: &Value) -> Result<Value, HostError> {
        let query = params["query"]
            .as_str()
            .ok_or_else(|| HostError::failed("`query` must be a string"))?;
        let words = search::words(query);
        if words.is_empty() {
            return Err(HostError::failed("query is empty"));
        }
        let cell_type = params["cell_type"].as_str();
        if let Some(t) = cell_type {
            if !matches!(t, "code" | "markdown" | "raw") {
                return Err(HostError::failed("cell_type must be code, markdown or raw"));
            }
        }
        let dir = self.checked_path(who, params, "dir")?;
        let scope = search::Scope {
            outputs: params["outputs"].as_bool().unwrap_or(false),
            cell_type,
        };
        let limit = params["limit"].as_u64().unwrap_or(20).clamp(1, 200) as usize;
        let keep = |p: &str, is_dir: bool| {
            if is_dir {
                self.may_enter(who, p)
            } else {
                self.may_read(who, p)
            }
        };
        let found = search_notebooks(&self.state, &dir, &words, scope, limit, &keep)
            .await
            .map_err(HostError::failed)?;
        let hits: Vec<Value> = found
            .notebooks
            .iter()
            .flat_map(|(path, title, hits)| {
                hits.iter().map(move |h| {
                    json!({"path": path, "title": title, "index": h.index, "id": h.id,
                           "cell_type": h.cell_type, "in": h.r#in, "excerpt": h.excerpt})
                })
            })
            .collect();
        Ok(json!({"hits": hits, "total": found.total, "truncated": found.truncated}))
    }
}

#[async_trait]
impl Responder for HostApi {
    async fn hello(&self, caller: &Caller) -> Hello {
        let who = self.who(caller).await;
        let mut methods: Vec<String> = READS.iter().map(|m| m.to_string()).collect();
        if !who.run.is_empty() && who.depth < MAX_DEPTH {
            methods.push("notebooks.run".into());
        }
        if who.edit_self && who.notebook.is_some() {
            methods.extend(SELF_EDITS.iter().map(|m| m.to_string()));
        }
        Hello {
            notebook: who.notebook,
            methods,
        }
    }

    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, HostError> {
        let who = self.who(caller).await;
        match method {
            "notebooks.list" => self.list(&who, &params).await,
            "notebooks.read" => self.read(&who, &params).await,
            "notebooks.results" => self.results(&who, &params).await,
            "notebooks.search" => self.search(&who, &params).await,
            "notebooks.run" => self.run(caller, &who, params).await,
            m if SELF_EDITS.contains(&m) => {
                if !who.edit_self {
                    return Err(HostError::denied(
                        "this notebook's edit_self grant is off (`[host] edit_self` or `ganymede grant`), so it can't edit its own cells",
                    ));
                }
                let Some(path) = who.notebook() else {
                    return Err(HostError::unavailable("this kernel has no notebook"));
                };
                self.edit_own(&caller.kernel_id, path, m, &params).await
            }
            _ => Err(HostError::unavailable(format!("no host method {method}"))),
        }
    }
}

/// Why a `self.*` edit was refused, inside the room.
enum Refused {
    NoCell(String),
    Mismatch(String),
    Bad(String),
}

/// `metadata.ganymede.agent.by` on cells a notebook's own code adds or
/// edits through `self.*` (#768), so they can't pass for a person's.
const KERNEL: &str = "kernel";

/// `params[key]` as a string, or `failed`.
fn string_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, HostError> {
    params[key]
        .as_str()
        .ok_or_else(|| HostError::failed(format!("`{key}` must be a string")))
}

/// Room holds by kernel id: `(room id, hold)`.
type Holds = Arc<std::sync::Mutex<std::collections::HashMap<String, (String, Hold)>>>;

impl HostApi {
    /// Keep room `room_id` open for `kernel_id` while the kernel runs, as
    /// an MCP connection keeps the rooms it edits (#768). A kernel with no
    /// client on its notebook would otherwise let the room retire between
    /// calls, and a notebook without cell ids (nbformat 4.4 and older)
    /// would get new ones, so an id `self.add` returned would stop
    /// naming its cell. The hold is let go when the kernel ends: a server
    /// kernel's when the kernel manager shuts it down, a pinned one's at
    /// [`HostApi::unpin`].
    async fn hold(&self, kernel_id: &str, room_id: &str) {
        {
            let mut holds = self.holds.lock().unwrap();
            holds.retain(|k, _| self.is_live(k));
            if holds
                .get(kernel_id)
                .is_some_and(|(r, h)| r == room_id && h.is_live())
            {
                return;
            }
        }
        // Watched from before the room loads, so a kernel shut down
        // meanwhile still lets its hold go: the receiver sees it at once.
        let alive = self
            .state
            .kernels
            .get(kernel_id)
            .ok()
            .map(|k| k.watch_alive());
        if let Some(h) = hold_notebook(&self.state.collab, &self.state.contents, room_id).await {
            // One made meanwhile by another call is replaced; it leaves the
            // room as this one joins it.
            let first = self
                .holds
                .lock()
                .unwrap()
                .insert(kernel_id.to_string(), (room_id.to_string(), h))
                .is_none();
            // One watcher per kernel: a later hold on another room
            // replaces the entry the watcher removes.
            if !self.is_live(kernel_id) {
                // Unpinned, or shut down, while the room loaded.
                self.holds.lock().unwrap().remove(kernel_id);
                return;
            }
            if first {
                if let Some(mut alive) = alive {
                    let holds = Arc::downgrade(&self.holds);
                    let kernel_id = kernel_id.to_string();
                    tokio::spawn(async move {
                        // Err: the kernel was dropped, so it ended too.
                        let _ = alive.wait_for(|a| !*a).await;
                        if let Some(holds) = holds.upgrade() {
                            holds.lock().unwrap().remove(&kernel_id);
                        }
                    });
                }
            }
        }
    }

    /// `self.add`, `self.edit` or `self.delete` by kernel `kernel_id` on
    /// `path`, its own notebook, through its room.
    async fn edit_own(
        &self,
        kernel_id: &str,
        path: &str,
        method: &str,
        params: &Value,
    ) -> Result<Value, HostError> {
        enum Op {
            Add {
                cell: Value,
                after: Option<String>,
            },
            Edit {
                id: String,
                source: String,
                hash: String,
            },
            Delete {
                id: String,
                hash: String,
            },
        }
        let op = match method {
            "self.add" => {
                let source = string_param(params, "source")?.to_string();
                let id = crate::mcp::new_id();
                let cell_type = match &params["cell_type"] {
                    Value::Null => "markdown",
                    Value::String(t) => t.as_str(),
                    _ => "",
                };
                // Marked as the notebook's own code's, so it can't pass for
                // a person's cell.
                let metadata = json!({"ganymede": {"agent": {
                    "by": KERNEL, "mode": "note", "at": room_ops::now()}}});
                let cell = room_ops::new_cell(cell_type, &id, &source, metadata)
                    .ok_or_else(|| HostError::failed("cell_type must be code, markdown or raw"))?;
                let after = match &params["after"] {
                    Value::Null => None,
                    Value::String(a) => Some(a.clone()),
                    _ => return Err(HostError::failed("`after` must be a cell id")),
                };
                Op::Add { cell, after }
            }
            "self.edit" => Op::Edit {
                id: string_param(params, "id")?.to_string(),
                source: string_param(params, "source")?.to_string(),
                hash: string_param(params, "source_hash")?.to_string(),
            },
            _ => Op::Delete {
                id: string_param(params, "id")?.to_string(),
                hash: string_param(params, "source_hash")?.to_string(),
            },
        };
        let state = &self.state;
        let room_id = notebook_room_id(&state.collab, &state.contents, path)
            .await
            .map_err(|e| HostError::failed(format!("cannot open {path}: {e}")))?;
        self.hold(kernel_id, &room_id).await;
        let outcome = with_notebook(&state.collab, &state.contents, &room_id, move |room| {
            if !room_ops::writable(room) {
                return (
                    Err(Refused::Bad(room_ops::READ_ONLY.into())),
                    Default::default(),
                );
            }
            room.edit(|txn, roots| {
                let check =
                    |txn: &mut ganymede_documents::yrs::TransactionMut, id: &str, hash: &str| {
                        let cell = schema::find_cell(txn, roots, id)
                            .ok_or_else(|| Refused::NoCell(id.into()))?;
                        let source = room_ops::source_of(txn, &cell).unwrap_or_default();
                        let now = ganymede_documents::murmur2(source.as_bytes(), 0);
                        if now.to_string() != hash {
                            return Err(Refused::Mismatch(id.into()));
                        }
                        Ok(cell)
                    };
                match op {
                    Op::Add { cell, after } => {
                        let id = cell["id"].clone();
                        room_ops::insert(txn, roots, cell, after.as_deref())
                            .map_err(Refused::NoCell)?;
                        Ok(json!({ "id": id }))
                    }
                    Op::Edit { id, source, hash } => {
                        check(txn, &id, &hash)?;
                        // Stamped as the kernel's, with the replaced source
                        // in its history, as an agent's edit is.
                        room_ops::set_source(txn, roots, &id, &source, KERNEL);
                        Ok(json!({}))
                    }
                    Op::Delete { id, hash } => {
                        check(txn, &id, &hash)?;
                        room_ops::delete(txn, roots, &id);
                        Ok(json!({}))
                    }
                }
            })
        })
        .await
        .ok_or_else(|| HostError::failed(format!("cannot open {path}")))?;
        outcome.map_err(|r| match r {
            Refused::NoCell(id) => HostError::failed(format!("no cell {id}")),
            Refused::Mismatch(id) => HostError::failed(format!(
                "source_mismatch: cell {id} changed since it was read; read it again"
            )),
            Refused::Bad(m) => HostError::failed(m),
        })
    }
}

/// The notebook at the request's path: its file id, made the first time.
/// A path that isn't a notebook is a 404.
async fn grant_target(state: &AppState, path: &str) -> Result<(String, String), HttpError> {
    let path = ganymede_contents::normalize_api_path(path);
    let options = GetOptions {
        content: false,
        type_hint: Some(ContentType::Notebook),
        ..Default::default()
    };
    let (id, _) = state
        .collab
        .index_existing(&state.contents, &path, options)
        .await
        .map_err(|e| match e {
            ganymede_contents::Error::NotFound(_) => {
                HttpError(404, format!("No notebook at {path:?}"))
            }
            e => e.into(),
        })?;
    Ok((id, path))
}

/// The answer to every `/api/host/grants` call: the notebook's own grant
/// (`{}` when none) and the `[host]` defaults it replaces key by key.
async fn grant_answer(state: &AppState, path: &str) -> Result<Json<Value>, HttpError> {
    let grant: Grant = match state.collab.grant(path).await {
        Some(text) => serde_json::from_str(&text).unwrap_or_default(),
        None => Grant::default(),
    };
    Ok(Json(json!({
        "path": path,
        "grant": grant,
        "defaults": *state.host,
    })))
}

/// `GET /api/host/grants/{path}`: a notebook's host API grant.
pub async fn get_grant(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Json<Value>, HttpError> {
    let (_, path) = grant_target(&state, &path).await?;
    grant_answer(&state, &path).await
}

/// `PUT /api/host/grants/{path}`: stores a notebook's grant, as
/// `ganymede grant` does, in place of the one it had. `{}` removes it.
pub async fn put_grant(
    State(state): State<AppState>,
    Path(path): Path<String>,
    LenientJson(body): LenientJson<Value>,
) -> Result<Json<Value>, HttpError> {
    let Some(keys) = body.as_object() else {
        return Err(HttpError(400, "A grant is a JSON object".into()));
    };
    if let Some(k) = keys
        .keys()
        .find(|k| !["read", "edit_self", "run"].contains(&k.as_str()))
    {
        return Err(HttpError(
            400,
            format!("Unknown grant key {k:?}: a grant has read, edit_self and run"),
        ));
    }
    let mut grant: Grant =
        serde_json::from_value(body.clone()).map_err(|e| HttpError(400, e.to_string()))?;
    // As `ganymede grant`: no `..`, which would read as narrow and could
    // resolve to the whole root.
    let normalize = |paths: &mut Option<Vec<String>>| -> Result<(), HttpError> {
        for p in paths.iter_mut().flatten() {
            if p.split(['/', '\\']).any(|c| c == "..") {
                return Err(HttpError(400, format!("{p}: `..` is not allowed")));
            }
            *p = ganymede_contents::normalize_api_path(p);
        }
        Ok(())
    };
    normalize(&mut grant.read)?;
    normalize(&mut grant.run)?;
    store_grant(&state, &path, grant).await
}

/// `DELETE /api/host/grants/{path}`: removes a notebook's grant, so it
/// gets the `[host]` defaults.
pub async fn delete_grant(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Json<Value>, HttpError> {
    store_grant(&state, &path, Grant::default()).await
}

async fn store_grant(state: &AppState, path: &str, grant: Grant) -> Result<Json<Value>, HttpError> {
    if !state.collab.is_persistent().await {
        return Err(HttpError(
            409,
            "This server keeps file ids in memory only, so a grant wouldn't outlive it".into(),
        ));
    }
    let (id, path) = grant_target(state, path).await?;
    let text = (!grant.is_empty())
        .then(|| serde_json::to_string(&grant))
        .transpose()
        .map_err(|e| HttpError(500, e.to_string()))?;
    state
        .collab
        .set_grant(id, text)
        .await
        .map_err(|e| HttpError(500, e))?;
    grant_answer(state, &path).await
}

#[cfg(test)]
mod tests {
    use super::intersect;

    fn v(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn intersect_keeps_what_both_grants_cover() {
        // The root and anything: the narrower side.
        assert_eq!(intersect(&v(&[""]), &v(&["journal"])), v(&["journal"]));
        assert_eq!(intersect(&v(&["journal"]), &v(&[""])), v(&["journal"]));
        // A folder and a path under it: the path.
        assert_eq!(
            intersect(&v(&["journal"]), &v(&["journal/2026", "reports"])),
            v(&["journal/2026"])
        );
        // Whole segments: journal doesn't cover journal-private.
        assert!(intersect(&v(&["journal"]), &v(&["journal-private"])).is_empty());
        // Nothing on either side is nothing.
        assert!(intersect(&v(&[]), &v(&[""])).is_empty());
    }
}
