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
//!
//! Reads reach the paths in the `[host]` `read` grant (the whole root by
//! default), matched by whole path segments against where a path leads
//! once symlinks are resolved; a notebook may always read itself. A
//! kernel's notebook is the one of the session the server started it for.

use crate::mcp::{notebook_for_search, search, search_notebooks};
use crate::AppState;
use async_trait::async_trait;
use ganymede_contents::{ContentType, GetOptions};
use ganymede_host::{Caller, Hello, HostError, Responder};
use serde::Deserialize;
use serde_json::{json, Value};

/// The `[host]` table of the config file.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
    /// API paths the `notebooks.*` reads may reach; `""` is the root.
    pub read: Vec<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            read: vec![String::new()],
        }
    }
}

const METHODS: [&str; 3] = ["notebooks.list", "notebooks.read", "notebooks.search"];

pub struct HostApi {
    state: AppState,
    read: Vec<String>,
    /// Each kernel's notebook session, as first seen: `hello` at kernel
    /// start sees the session the kernel was started for, and a notebook
    /// that joins the kernel later doesn't take its place.
    sessions: std::sync::Mutex<std::collections::HashMap<String, String>>,
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
            sessions: Default::default(),
        }
    }

    /// The API path of the notebook `kernel_id` was started for: its
    /// first notebook session's.
    fn notebook(&self, kernel_id: &str) -> Option<String> {
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

    /// Whether the grant covers the whole root, so no path needs checking.
    fn reads_everything(&self) -> bool {
        self.read.iter().any(|g| g.is_empty())
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
    fn may_read(&self, notebook: Option<&str>, path: &str) -> bool {
        if notebook == Some(path) || self.reads_everything() {
            return true;
        }
        let Some(real) = self.resolved(path) else {
            return false;
        };
        // Grants resolved the same way, so a granted folder that is itself
        // a symlink still covers what is in it.
        self.read.iter().any(|g| {
            ganymede_contents::path_under(&real, g).is_some()
                || self
                    .resolved(g)
                    .is_some_and(|g| ganymede_contents::path_under(&real, &g).is_some())
        })
    }

    /// Whether a directory may be listed or walked: it is readable, or a
    /// grant or the notebook itself lies under it.
    fn may_enter(&self, notebook: Option<&str>, dir: &str) -> bool {
        self.may_read(notebook, dir)
            || notebook.is_some_and(|n| ganymede_contents::path_under(n, dir).is_some())
            || self
                .read
                .iter()
                .any(|g| ganymede_contents::path_under(g, dir).is_some())
    }

    fn checked_path(
        &self,
        notebook: Option<&str>,
        params: &Value,
        key: &str,
    ) -> Result<String, HostError> {
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
            self.may_enter(notebook, &path)
        } else {
            self.may_read(notebook, &path)
        };
        if !allowed {
            return Err(HostError::denied(format!(
                "{} is outside this notebook's read grant",
                if path.is_empty() { "the root" } else { &path }
            )));
        }
        Ok(path)
    }

    async fn list(&self, notebook: Option<&str>, params: &Value) -> Result<Value, HostError> {
        let dir = self.checked_path(notebook, params, "dir")?;
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
                        if item["name"] != ".ipynb_checkpoints"
                            && self.may_enter(notebook, &path) =>
                    {
                        out.push(json!({"path": path, "type": "directory", "modified": modified}));
                    }
                    "notebook" if self.may_read(notebook, &path) => {
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

    async fn read(&self, notebook: Option<&str>, params: &Value) -> Result<Value, HostError> {
        let path = self.checked_path(notebook, params, "path")?;
        let nb = notebook_for_search(&self.state, &path)
            .await
            .ok_or_else(|| HostError::failed(format!("cannot read {path} as a notebook")))?;
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

    async fn search(&self, notebook: Option<&str>, params: &Value) -> Result<Value, HostError> {
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
        let dir = self.checked_path(notebook, params, "dir")?;
        let scope = search::Scope {
            outputs: params["outputs"].as_bool().unwrap_or(false),
            cell_type,
        };
        let limit = params["limit"].as_u64().unwrap_or(20).clamp(1, 200) as usize;
        let keep = |p: &str, is_dir: bool| {
            if is_dir {
                self.may_enter(notebook, p)
            } else {
                self.may_read(notebook, p)
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
        Hello {
            notebook: self.notebook(&caller.kernel_id),
            methods: METHODS.iter().map(|m| m.to_string()).collect(),
        }
    }

    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, HostError> {
        let notebook = self.notebook(&caller.kernel_id);
        let notebook = notebook.as_deref();
        match method {
            "notebooks.list" => self.list(notebook, &params).await,
            "notebooks.read" => self.read(notebook, &params).await,
            "notebooks.search" => self.search(notebook, &params).await,
            _ => Err(HostError::unavailable(format!("no host method {method}"))),
        }
    }
}
