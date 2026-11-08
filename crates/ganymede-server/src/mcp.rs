//! `ganymede mcp`: notebooks for agents, over the Model Context Protocol
//! (#245).
//!
//! The tools work on document rooms ([`crate::collab`]) and the server's
//! kernels, so an agent's edits and outputs show up live in JupyterLab
//! (with the collaboration extensions) and are saved like anyone else's.
//! They are shaped around journaling an analysis as it happens: open a
//! notebook, run code that is recorded as cells, add Markdown narration,
//! read back a compact view. Cells are named by their stable ids.
//!
//! Served two ways: `/mcp` on `ganymede serve --mcp` (streamable HTTP,
//! behind the token and the `Host` check), and `ganymede mcp` over stdio,
//! which uses a running server's `/mcp` or runs one in-process.
//!
//! What goes back to the model follows nb-cli's format: an `@@cell` line
//! with the cell's index, id and type, its source, and an `@@output` line
//! per output with plain text after it. HTML is stripped, tracebacks lose
//! their ANSI colours and keep the last frames, and an output longer than
//! 4,000 characters is written to a file named by its sha256, whose path
//! is given instead. Images are sent as image content only with
//! `--images`.
//!
//! Cells an agent writes carry `metadata.ganymede.agent = {"by", "mode",
//! "at"}`: `by` is the client's name, `mode` is `executed` (ran in the
//! kernel), `recorded` (a step that ran elsewhere) or `note`. Scratch runs
//! (`cell_run` with `record: false`) run in the kernel, sharing its state,
//! and leave nothing in the notebook. `cell_edit` keeps the source it
//! replaces in `history` (the last [`HISTORY_LIMIT`] of them).
//!
//! `notebook_read` and `notebook_open` end with a read mark, which
//! `notebook_read(since)` takes to list only what others changed since
//! (see the `marks` module).

mod marks;

use crate::collab::{hold_notebook, notebook_room_id, with_notebook, Hold};
use crate::svg;
use crate::AppState;
use base64::Engine as _;
use ganymede_api::sessions::KernelRef;
use ganymede_api::SessionCreate;
use ganymede_contents::{ContentType, GetOptions, SaveRequest};
use ganymede_documents::schema::{self, Roots};
use ganymede_documents::yrs::{Array, GetString, Map, Out, TransactionMut};
use ganymede_documents::{Effects, Room};
use ganymede_nbformat::{Cell, Notebook, Output};
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{prompt, prompt_handler, prompt_router, schemars, tool, tool_handler, tool_router};
use rmcp::{ErrorData as McpError, Peer, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Outputs longer than this go to a file.
const INLINE_LIMIT: usize = 4000;
/// Traceback frames kept, from the end.
const TRACEBACK_FRAMES: usize = 5;
/// How long `cell_run` waits by default.
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// How `ganymede mcp` and `/mcp` behave.
#[derive(Debug, Clone, Default)]
pub struct McpOptions {
    /// No notebook edits, no execution.
    pub read_only: bool,
    /// Edits, but no execution.
    pub no_execute: bool,
    /// Send images to the model as image content.
    pub images: bool,
    /// Where long outputs go; the system temp dir when `None`.
    pub outputs_dir: Option<PathBuf>,
}

/// A server's `/mcp`: its options, and its connections' read marks.
pub struct McpState {
    opts: Arc<McpOptions>,
    marks: Arc<Mutex<marks::Marks>>,
}

impl McpState {
    pub fn new(opts: McpOptions) -> Self {
        McpState {
            opts: Arc::new(opts),
            marks: Arc::default(),
        }
    }
}

/// How long an MCP session may go without a request before it ends.
/// `ganymede mcp` starts a new one when the server has ended its own.
const SESSION_IDLE: Duration = Duration::from_secs(60 * 60);

/// `/mcp`, streamable HTTP, behind the API's auth. The `Host` check is the
/// server's own (`allow_remote_access`), so rmcp's is off.
pub fn router(state: AppState, mcp: &McpState) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    let opts = mcp.opts.clone();
    let marks = mcp.marks.clone();
    let app_state = state.clone();
    // rmcp ends a session after 5 idle minutes; an agent often waits
    // longer on its user. A session holds its notebooks' rooms open, so
    // the limit stays, for clients that never end theirs.
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(SESSION_IDLE);
    let service: StreamableHttpService<Notebooks, LocalSessionManager> = StreamableHttpService::new(
        move || {
            Ok(Notebooks::new(
                app_state.clone(),
                opts.clone(),
                marks.clone(),
            ))
        },
        Arc::new(sessions),
        StreamableHttpServerConfig::default().disable_allowed_hosts(),
    );
    axum::Router::new()
        .route_service("/mcp", service)
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::api,
        ))
}

/// The MCP server: one per connection (MCP session).
#[derive(Clone)]
pub struct Notebooks {
    state: AppState,
    opts: Arc<McpOptions>,
    /// The notebook this connection last opened, for tools called
    /// without `path`. Per connection, so agents sharing a server don't
    /// redirect each other.
    current: Arc<Mutex<Option<String>>>,
    /// The rooms of notebooks this connection used, by room id, kept open
    /// while it lasts so cell ids stay the same: a notebook without ids
    /// (nbformat 4.4 and older) gets new ones each time its room opens.
    holds: Arc<Mutex<HashMap<String, Hold>>>,
    /// Every connection's read marks, shared across the server.
    marks: Arc<Mutex<marks::Marks>>,
    /// This connection's read mark token for each notebook it read.
    tokens: Arc<Mutex<HashMap<String, String>>>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

fn err(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

fn text(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(s)]))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenArgs {
    /// Notebook path relative to the server root, such as `analysis/run.ipynb`.
    pub path: String,
    /// Kernelspec name for a new notebook or a new kernel (default: the
    /// notebook's own, else the server's default).
    pub kernel: Option<String>,
    /// For a new notebook: a title, written as a first Markdown cell.
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// Notebook path; the one last opened when left out.
    pub path: Option<String>,
    /// A read mark from an earlier read's `@@read` line: return only the
    /// cells others added, edited, deleted or moved since that read.
    /// Without `path`, the mark's notebook. The mark becomes this
    /// connection's for that notebook, so its own edits move it.
    pub since: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunArgs {
    /// Code to run.
    pub source: String,
    /// Insert after this cell id (default: at the end).
    pub after: Option<String>,
    /// Record the run as a cell (default true). `false` is a scratch run:
    /// it runs in the kernel and leaves nothing in the notebook, but its
    /// variables stay defined in the kernel.
    pub record: Option<bool>,
    /// Seconds to wait for the result (default 600). A recorded cell
    /// keeps running after a timeout; a scratch run is interrupted.
    pub timeout: Option<u64>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CellKind {
    Markdown,
    Code,
    Raw,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AddMode {
    /// Narration.
    Note,
    /// A step that ran outside the kernel (a shell command, a file the
    /// agent wrote), recorded so the notebook tells the whole story.
    Recorded,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AddArgs {
    pub source: String,
    /// Default markdown.
    pub cell_type: Option<CellKind>,
    /// Default note.
    pub mode: Option<AddMode>,
    /// Insert after this cell id (default: at the end).
    pub after: Option<String>,
    pub path: Option<String>,
    /// A read mark (from an `@@read` line) to move past the new cell, as
    /// if its reader had added it: for a hook adding a cell on an agent's
    /// behalf, so the agent isn't told of it as someone else's.
    pub mark: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    pub id: String,
    pub source: String,
    /// Run the cell after editing it (code cells).
    pub run: Option<bool>,
    pub timeout: Option<u64>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IdArgs {
    pub id: String,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MoveArgs {
    pub id: String,
    /// Put the cell after this one; at the top when left out.
    pub after: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KernelAction {
    Interrupt,
    Restart,
    Shutdown,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KernelArgs {
    pub action: KernelAction,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunAllArgs {
    /// Restart the kernel first (default true).
    pub restart: Option<bool>,
    pub timeout: Option<u64>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArgs {
    /// Directory relative to the server root (default: the root).
    pub dir: Option<String>,
}

#[tool_router]
impl Notebooks {
    pub fn new(state: AppState, opts: Arc<McpOptions>, marks: Arc<Mutex<marks::Marks>>) -> Self {
        Notebooks {
            state,
            opts,
            current: Arc::default(),
            holds: Arc::default(),
            marks,
            tokens: Arc::default(),
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    #[tool(
        description = "Open a notebook, creating it if it doesn't exist, and start or join its kernel. Later calls may leave out `path`. Paths are relative to the server's root directory, which the reply names. Returns a compact view of the notebook."
    )]
    async fn notebook_open(
        &self,
        Parameters(a): Parameters<OpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        let path = ganymede_contents::normalize_api_path(&a.path);
        if !path.ends_with(".ipynb") {
            return Err(err("path must end in .ipynb"));
        }
        // Create only what isn't there; another error (permissions, I/O)
        // must not lead to writing over the file.
        let exists = match self.state.contents.get(&path, GetOptions::default()).await {
            Ok(_) => true,
            Err(ganymede_contents::Error::NotFound(_)) => false,
            Err(e) => return Err(err(format!("cannot open {path}: {e}"))),
        };
        if !exists {
            if self.opts.read_only {
                return Err(err(format!("{path} doesn't exist (read-only)")));
            }
            self.create(&path, a.kernel.as_deref(), a.title.as_deref())
                .await?;
        }
        *self.current.lock().expect("current") = Some(path.clone());
        let kernel = if self.opts.read_only || self.opts.no_execute {
            None
        } else {
            Some(self.kernel_for(&path, a.kernel.as_deref()).await?)
        };
        let (nb, token) = self.read_marked(&path).await?;
        let root = match self.state.collab.root() {
            Some(r) => format!(" (server root {})", r.display()),
            None => String::new(),
        };
        let mut out = format!(
            "{} {path}{root}{}\n\n",
            if exists { "Opened" } else { "Created" },
            match &kernel {
                Some((_, name)) => format!(" on kernel {name}"),
                None => String::new(),
            }
        );
        out.push_str(&self.render(&nb, true));
        out.push_str(&format!("@@read {token}\n"));
        text(out)
    }

    #[tool(
        description = "Read a notebook as text: cells with their ids, sources and outputs, then an `@@read <mark>` line. With `since: <mark>`, return only the cells others (a person, another agent) added, edited, deleted or moved since the read that gave the mark; your own edits through these tools are left out. Outputs are not compared."
    )]
    async fn notebook_read(
        &self,
        Parameters(a): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        // With a mark and no path, the mark's own notebook.
        let mark_path = match (&a.path, &a.since) {
            (None, Some(since)) => self
                .marks
                .lock()
                .expect("marks")
                .get(since)
                .map(|m| m.path.clone()),
            _ => None,
        };
        let path = match mark_path {
            Some(p) => p,
            None => self.path(a.path)?,
        };
        let Some(since) = a.since else {
            let (nb, token) = self.read_marked(&path).await?;
            return text(format!("{}@@read {token}\n", self.render(&nb, true)));
        };
        match self.read_since(&path, &since).await? {
            Some((nb, changes)) => {
                // The mark is this connection's now, so its own edits
                // move it (as after `ganymede mcp` starts a new session).
                self.tokens
                    .lock()
                    .expect("tokens")
                    .insert(path.clone(), since.clone());
                text(self.render_changes(&nb, &changes, &since))
            }
            None => {
                let (nb, token) = self.read_marked(&path).await?;
                text(format!(
                    "Read mark {since} is unknown here for {path} (the server restarted, \
                     it was dropped, or it is for another path); the whole notebook follows.\n\n{}@@read {token}\n",
                    self.render(&nb, true)
                ))
            }
        }
    }

    #[tool(
        description = "Run code in the notebook's kernel and return its outputs. By default the code is recorded as a new code cell (at the end, or after `after`); `record: false` runs it as scratch work that leaves nothing in the notebook and doesn't advance the execution count; it shares the kernel's state, so variables it assigns stay defined."
    )]
    async fn cell_run(
        &self,
        Parameters(a): Parameters<RunArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.can_execute()?;
        let path = self.path(a.path)?;
        let timeout = Duration::from_secs(a.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        let (kernel_id, _) = self.kernel_for(&path, None).await?;
        if !a.record.unwrap_or(true) {
            return self.scratch(&kernel_id, a.source, timeout).await;
        }
        self.can_edit()?;
        let id = new_id();
        let cell = json!({
            "cell_type": "code", "execution_count": null, "id": id,
            "metadata": provenance(&context.peer, "executed"),
            "outputs": [], "source": a.source,
        });
        self.insert(&path, cell, a.after, None).await?;
        self.run_cells(&path, &kernel_id, vec![id.clone()], timeout)
            .await
    }

    #[tool(
        description = "Add a cell without running it: Markdown narration (the default), or with `mode: recorded` a step that ran outside the kernel. Returns the new cell's id."
    )]
    async fn cell_add(
        &self,
        Parameters(a): Parameters<AddArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.can_edit()?;
        let path = self.path(a.path)?;
        let id = new_id();
        let mode = match a.mode {
            Some(AddMode::Recorded) => "recorded",
            _ => "note",
        };
        let metadata = provenance(&context.peer, mode);
        let cell = match a.cell_type.unwrap_or(CellKind::Markdown) {
            CellKind::Code => json!({"cell_type": "code", "execution_count": null, "id": id,
                "metadata": metadata, "outputs": [], "source": a.source}),
            CellKind::Markdown => {
                json!({"cell_type": "markdown", "id": id, "metadata": metadata, "source": a.source})
            }
            CellKind::Raw => {
                json!({"cell_type": "raw", "id": id, "metadata": metadata, "source": a.source})
            }
        };
        self.insert(&path, cell, a.after, a.mark).await?;
        text(format!("Added cell {id}"))
    }

    #[tool(
        description = "Replace a cell's source; with `run: true`, run it after. The replaced source is kept in the cell's metadata.ganymede.agent.history; to remove a secret, delete the cell instead."
    )]
    async fn cell_edit(
        &self,
        Parameters(a): Parameters<EditArgs>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.can_edit()?;
        let run = a.run.unwrap_or(false);
        if run {
            self.can_execute()?;
        }
        let path = self.path(a.path)?;
        let by = client_name(&context.peer);
        let id = a.id.clone();
        let source = a.source;
        let found = self
            .own_edit(
                &path,
                vec![a.id.clone()],
                marks::Aspect::Source,
                None,
                move |room| {
                    room.edit(|txn, roots| {
                        let Some(cell) = schema::find_cell(txn, roots, &id) else {
                            return false;
                        };
                        let previous = match cell.get(txn, "source") {
                            Some(Out::YText(t)) => {
                                let previous = t.get_string(txn);
                                schema::set_text(txn, &t, &source);
                                Some(previous)
                            }
                            other => {
                                // Not a shared string: keep its text if it has any.
                                let previous = other
                                    .map(|o| schema::out_to_json(txn, &o))
                                    .and_then(|v| match v {
                                        Value::String(s) => Some(s),
                                        Value::Array(a) => Some(
                                            a.iter().filter_map(Value::as_str).collect::<String>(),
                                        ),
                                        _ => None,
                                    });
                                cell.insert(
                                    txn,
                                    "source",
                                    ganymede_documents::yrs::TextPrelim::new(source.clone()),
                                );
                                previous
                            }
                        };
                        // An unchanged source is no edit: the cell keeps its
                        // provenance. An empty one replaced is no revision.
                        if previous.as_deref() != Some(source.as_str()) {
                            let superseded = previous.filter(|p| !p.is_empty());
                            stamp(txn, &cell, &by, superseded);
                        }
                        true
                    })
                },
            )
            .await?;
        if !found {
            return Err(err(format!("no cell {}", a.id)));
        }
        if !run {
            return text(format!("Edited cell {}", a.id));
        }
        let (kernel_id, _) = self.kernel_for(&path, None).await?;
        let timeout = Duration::from_secs(a.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        self.run_cells(&path, &kernel_id, vec![a.id], timeout).await
    }

    #[tool(description = "Delete a cell.")]
    async fn cell_delete(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.can_edit()?;
        let path = self.path(a.path)?;
        let id = a.id.clone();
        let found = self
            .own_edit(
                &path,
                vec![a.id.clone()],
                marks::Aspect::All,
                None,
                move |room| {
                    room.edit(|txn, roots| match schema::cell_index(txn, roots, &id) {
                        Some(i) => {
                            roots.cells.remove(txn, i);
                            true
                        }
                        None => false,
                    })
                },
            )
            .await?;
        if !found {
            return Err(err(format!("no cell {}", a.id)));
        }
        text(format!("Deleted cell {}", a.id))
    }

    #[tool(description = "Move a cell after another one, or to the top.")]
    async fn cell_move(
        &self,
        Parameters(a): Parameters<MoveArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.can_edit()?;
        let path = self.path(a.path)?;
        let (id, after) = (a.id.clone(), a.after.clone());
        let moved = self
            .own_edit(
                &path,
                vec![a.id.clone()],
                marks::Aspect::Place,
                None,
                move |room| {
                    room.edit(|txn, roots| {
                        let Some(from) = schema::cell_index(txn, roots, &id) else {
                            return Err(format!("no cell {id}"));
                        };
                        // Where it goes, as an index before the cell is taken
                        // out; checked first, so a bad `after` changes nothing.
                        let to = match &after {
                            None => 0,
                            Some(a) if *a == id => return Ok(false),
                            Some(a) => match schema::cell_index(txn, roots, a) {
                                Some(i) if i < from => i + 1,
                                Some(i) => i,
                                None => return Err(format!("no cell {a}")),
                            },
                        };
                        if to == from {
                            return Ok(false);
                        }
                        let Some(Out::YMap(cell)) = roots.cells.get(txn, from) else {
                            return Err(format!("no cell {id}"));
                        };
                        let value = schema::out_to_json(txn, &Out::YMap(cell));
                        roots.cells.remove(txn, from);
                        schema::insert_cell(txn, &roots.cells, to, value);
                        Ok(true)
                    })
                },
            )
            .await?;
        if moved.map_err(err)? {
            text(format!("Moved cell {}", a.id))
        } else {
            text(format!("Cell {} is already there", a.id))
        }
    }

    #[tool(description = "Interrupt, restart or shut down the notebook's kernel.")]
    async fn kernel_control(
        &self,
        Parameters(a): Parameters<KernelArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.can_execute()?;
        let path = self.path(a.path)?;
        // Only restart starts a kernel; there is nothing to interrupt or
        // shut down without one.
        let (kernel_id, name) = match a.action {
            KernelAction::Restart => self.kernel_for(&path, None).await?,
            _ => self
                .running_kernel(&path)
                .ok_or_else(|| err(format!("no kernel is running for {path}")))?,
        };
        let kernels = &self.state.kernels;
        let r = match a.action {
            KernelAction::Interrupt => kernels.interrupt(&kernel_id).await.map(|_| "Interrupted"),
            KernelAction::Restart => kernels.restart(&kernel_id).await.map(|_| "Restarted"),
            KernelAction::Shutdown => {
                // Deleting the session shuts its kernel down.
                for s in self.state.sessions.list() {
                    if s.kernel.id == kernel_id {
                        let _ = self.state.sessions.delete(&s.id).await;
                    }
                }
                match kernels.shutdown(&kernel_id).await {
                    Err(e) if e.status == 404 => Ok("Shut down"),
                    r => r.map(|_| "Shut down"),
                }
            }
        };
        let done = r.map_err(|e| err(e.message))?;
        text(format!("{done} kernel {name} ({kernel_id})"))
    }

    #[tool(
        description = "Run every code cell in order, after restarting the kernel unless `restart: false`, and return the notebook."
    )]
    async fn notebook_run_all(
        &self,
        Parameters(a): Parameters<RunAllArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.can_execute()?;
        let path = self.path(a.path)?;
        let (kernel_id, _) = self.kernel_for(&path, None).await?;
        if a.restart.unwrap_or(true) {
            self.state
                .kernels
                .restart(&kernel_id)
                .await
                .map_err(|e| err(e.message))?;
        }
        let nb = self.notebook(&path).await?;
        let ids: Vec<String> = nb
            .cells
            .iter()
            .filter(|c| matches!(c, Cell::Code(code) if !code.source.as_str().trim().is_empty()))
            .filter_map(|c| c.id().map(str::to_string))
            .collect();
        let timeout = Duration::from_secs(a.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        let finished = crate::execute::run_and_wait(&self.state, &path, &kernel_id, ids, timeout)
            .await
            .map_err(err)?;
        let nb = self.notebook(&path).await?;
        let mut out = String::new();
        if !finished {
            out.push_str("Timed out; cells are still running.\n\n");
        }
        out.push_str(&self.render(&nb, true));
        text(out)
    }

    #[tool(description = "List the kernels that can run notebooks here.")]
    async fn kernelspecs_list(&self) -> Result<CallToolResult, McpError> {
        let specs = self
            .state
            .kernels
            .kernelspecs()
            .map_err(|e| err(e.message))?;
        let mut out = format!("default: {}\n", specs.default);
        for (name, k) in &specs.kernelspecs {
            out.push_str(&format!(
                "{name}: {} ({})\n",
                k.spec.display_name, k.spec.language
            ));
        }
        text(out)
    }

    #[tool(description = "List notebooks and directories in a directory.")]
    async fn notebooks_list(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dir = ganymede_contents::normalize_api_path(a.dir.as_deref().unwrap_or(""));
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
            .map_err(|e| err(e.to_string()))?;
        let mut out = String::new();
        if let Some(Value::Array(items)) = model.content {
            for item in items {
                let kind = item["type"].as_str().unwrap_or_default();
                let path = item["path"].as_str().unwrap_or_default();
                match kind {
                    "directory" => out.push_str(&format!("{path}/\n")),
                    "notebook" => out.push_str(&format!("{path}\n")),
                    _ => {}
                }
            }
        }
        if out.is_empty() {
            out.push_str("(no notebooks)\n");
        }
        text(out)
    }
}

#[prompt_router]
impl Notebooks {
    /// How to journal an analysis into a notebook.
    #[prompt(name = "journal")]
    async fn journal(&self) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(Role::User, JOURNAL_PROMPT)]
    }
}

const JOURNAL_PROMPT: &str = "\
Keep a notebook as the record of this analysis while you work, not as a write-up at the end.

- Start with `notebook_open` (a new notebook gets a title cell); later tools may leave out `path`.
- Before each step, add a short Markdown note with `cell_add` saying what you are about to find out and why.
- Run the code that produces results with `cell_run`, so the notebook keeps both the code and its output.
- Use `cell_run` with `record: false` for checks that don't belong in the record (peeking at a value, trying a call).
- When a step happens outside the kernel (a shell command, a file you wrote), add it with `cell_add` and `mode: recorded`.
- After a result, add a Markdown note with what it shows. Fix a wrong cell with `cell_edit` rather than adding another.
- A person may edit the notebook while you work. Before building on it again, call `notebook_read` with `since` set to the mark from your last `@@read` line, and work from what they changed.
- End with a Markdown summary of what was found. `notebook_run_all` checks that the notebook runs top to bottom.";

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for Notebooks {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("ganymede", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Jupyter notebooks on a Ganymede server. Open one with notebook_open, \
             then journal your work into it: cell_run records code with its output, \
             cell_add adds Markdown notes. The `journal` prompt has the conventions.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let current = self.current.lock().expect("current").clone();
        let resources = current
            .into_iter()
            .map(|p| {
                Resource::new(format!("notebook://{p}"), p.clone()).with_mime_type("text/markdown")
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let Some(path) = request.uri.strip_prefix("notebook://") else {
            return Err(McpError::resource_not_found(
                format!("not a notebook:// URI: {}", request.uri),
                None,
            ));
        };
        // The tools' paths are normalized, so one file is one room.
        let path = ganymede_contents::normalize_api_path(path);
        if !path.ends_with(".ipynb") {
            return Err(McpError::resource_not_found(
                format!("not a notebook: {path}"),
                None,
            ));
        }
        let nb = self.notebook(&path).await?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            self.render(&nb, true),
            request.uri.clone(),
        )])
        .into())
    }
}

/// Client name for provenance.
fn client_name(peer: &Peer<RoleServer>) -> String {
    peer.peer_info()
        .map(|i| i.client_info.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "agent".into())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn provenance(peer: &Peer<RoleServer>, mode: &str) -> Value {
    json!({"ganymede": {"agent": {"by": client_name(peer), "mode": mode, "at": now()}}})
}

/// How many superseded sources a cell's `metadata.ganymede.agent.history`
/// keeps; older ones are dropped.
pub const HISTORY_LIMIT: usize = 10;

/// Update an existing cell's provenance after an edit: `at`, and `by` when
/// the cell had none. A `superseded` source is appended to `history` as
/// `{"source", "by", "at"}`: the text the edit replaced, who replaced it
/// and when.
fn stamp(
    txn: &mut TransactionMut,
    cell: &ganymede_documents::yrs::MapRef,
    by: &str,
    superseded: Option<String>,
) {
    let meta = match cell.get(txn, "metadata") {
        Some(Out::YMap(m)) => m,
        _ => cell.insert(
            txn,
            "metadata",
            ganymede_documents::yrs::MapPrelim::default(),
        ),
    };
    let mut g = meta
        .get(txn, "ganymede")
        .map(|o| schema::out_to_json(txn, &o))
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    // Indexing a non-object panics; another tool may have written one.
    if !g["agent"].is_object() {
        g["agent"] = json!({});
    }
    let agent = &mut g["agent"];
    if agent.get("by").is_none() {
        agent["by"] = json!(by);
        agent["mode"] = json!("note");
    }
    let at = now();
    if let Some(source) = superseded {
        let mut history = match agent["history"].take() {
            Value::Array(h) => h,
            _ => Vec::new(),
        };
        history.push(json!({"source": source, "by": by, "at": at}));
        let excess = history.len().saturating_sub(HISTORY_LIMIT);
        history.drain(..excess);
        agent["history"] = Value::Array(history);
    }
    agent["at"] = json!(at);
    meta.insert(txn, "ganymede", schema::json_to_any(&g));
}

impl Notebooks {
    fn can_edit(&self) -> Result<(), McpError> {
        if self.opts.read_only {
            return Err(err("this server is read-only"));
        }
        Ok(())
    }

    fn can_execute(&self) -> Result<(), McpError> {
        if self.opts.read_only || self.opts.no_execute {
            return Err(err("execution is turned off on this server"));
        }
        Ok(())
    }

    fn path(&self, path: Option<String>) -> Result<String, McpError> {
        match path {
            Some(p) => Ok(ganymede_contents::normalize_api_path(&p)),
            None => self
                .current
                .lock()
                .expect("current")
                .clone()
                .ok_or_else(|| err("no notebook open: call notebook_open first, or pass path")),
        }
    }

    async fn room<R: Send + 'static>(
        &self,
        path: &str,
        f: impl FnOnce(&mut Room) -> (R, Effects) + Send + 'static,
    ) -> Result<R, McpError> {
        // By room id, so a file recreated at the same path (a new room)
        // is held too; a room that stopped is held again.
        let room_id = notebook_room_id(&self.state.collab, &self.state.contents, path)
            .await
            .map_err(|e| err(format!("cannot open {path}: {e}")))?;
        let held = self
            .holds
            .lock()
            .expect("holds")
            .get(&room_id)
            .is_some_and(Hold::is_live);
        if !held {
            if let Some(h) = hold_notebook(&self.state.collab, &self.state.contents, path).await {
                // A hold made meanwhile by another call is replaced; it
                // leaves the room as this one joins it.
                self.holds.lock().expect("holds").insert(room_id, h);
            }
        }
        with_notebook(&self.state.collab, &self.state.contents, path, f)
            .await
            .ok_or_else(|| err(format!("cannot open {path}")))
    }

    async fn notebook(&self, path: &str) -> Result<Notebook, McpError> {
        self.room(path, |room| (room.notebook_with_ids(), Effects::default()))
            .await?
            .ok_or_else(|| err(format!("{path} is not a notebook")))?
            .map_err(|e| err(e.to_string()))
    }

    /// The notebook, and this connection's read mark moved to it.
    async fn read_marked(&self, path: &str) -> Result<(Notebook, String), McpError> {
        let token = self
            .tokens
            .lock()
            .expect("tokens")
            .entry(path.to_string())
            .or_insert_with(marks::token)
            .clone();
        let (marks, t, p) = (self.marks.clone(), token.clone(), path.to_string());
        // In the room, so no edit lands between the read and the mark.
        let nb = self
            .room(path, move |room| {
                let nb = room.notebook_with_ids();
                if let Some(Ok(nb)) = &nb {
                    marks
                        .lock()
                        .expect("marks")
                        .set(&t, &p, marks::snapshot(nb));
                }
                (nb, Effects::default())
            })
            .await?
            .ok_or_else(|| err(format!("{path} is not a notebook")))?
            .map_err(|e| err(e.to_string()))?;
        Ok((nb, token))
    }

    /// The notebook and what changed since read mark `since`, which then
    /// moves to it. `None` when the server has no such mark.
    async fn read_since(
        &self,
        path: &str,
        since: &str,
    ) -> Result<Option<(Notebook, marks::Changes)>, McpError> {
        match self.marks.lock().expect("marks").get(since) {
            // A mark for another path (the notebook was renamed, or the
            // caller mixed them up) is as good as none: the whole
            // notebook follows.
            Some(m) if m.path == path => {}
            _ => return Ok(None),
        }
        let (marks, t, p) = (self.marks.clone(), since.to_string(), path.to_string());
        let r = self
            .room(path, move |room| {
                let r = match room.notebook_with_ids() {
                    Some(Ok(nb)) => {
                        let now = marks::snapshot(&nb);
                        let mut marks = marks.lock().expect("marks");
                        match marks.get(&t).filter(|m| m.path == p) {
                            Some(m) => {
                                let changes = marks::diff(&m.cells, &now);
                                m.cells = now;
                                Ok(Some((nb, changes)))
                            }
                            None => Ok(None),
                        }
                    }
                    Some(Err(e)) => Err(e.to_string()),
                    None => Err("not a notebook".to_string()),
                };
                (r, Effects::default())
            })
            .await?;
        r.map_err(|e| err(format!("{path}: {e}")))
    }

    /// Edit the notebook's room with `f`, moving this connection's read
    /// mark past the change to cells `ids`, so the change isn't reported
    /// to it as someone else's: only the `aspect` the tool changes, and
    /// only when `f` changed the document (a failed or no-op edit adopts
    /// nothing, so someone else's change to the cell is still reported).
    /// `also` is another read mark to move the same way (`cell_add`'s
    /// `mark`), when it is for this notebook.
    async fn own_edit<R: Send + 'static>(
        &self,
        path: &str,
        ids: Vec<String>,
        aspect: marks::Aspect,
        also: Option<String>,
        f: impl FnOnce(&mut Room) -> (R, Effects) + Send + 'static,
    ) -> Result<R, McpError> {
        let mut tokens: Vec<String> = self
            .tokens
            .lock()
            .expect("tokens")
            .get(path)
            .cloned()
            .into_iter()
            .collect();
        tokens.extend(also.filter(|t| !tokens.contains(t)));
        let (marks, p) = (self.marks.clone(), path.to_string());
        self.room(path, move |room| {
            // Another server owns the file (#272): its room would undo
            // the edit and never save it, so refuse it here.
            if room.is_read_only() {
                return (None, Effects::default());
            }
            let (r, fx) = f(room);
            if tokens.is_empty() || !fx.changed {
                return (Some(r), fx);
            }
            if let Some(Ok(nb)) = room.notebook_with_ids() {
                let now = marks::snapshot(&nb);
                let mut marks = marks.lock().expect("marks");
                for token in &tokens {
                    if let Some(m) = marks.get(token).filter(|m| m.path == p) {
                        for id in &ids {
                            marks::adopt(&mut m.cells, &now, id, aspect);
                        }
                    }
                }
            }
            (Some(r), fx)
        })
        .await?
        .ok_or_else(|| {
            err(format!(
                "{path} is open in another server on this machine, so it is read-only here"
            ))
        })
    }

    async fn create(
        &self,
        path: &str,
        kernel: Option<&str>,
        title: Option<&str>,
    ) -> Result<(), McpError> {
        let specs = self
            .state
            .kernels
            .kernelspecs()
            .map_err(|e| err(e.message))?;
        let name = kernel.unwrap_or(&specs.default);
        let mut metadata = json!({});
        if let Some(spec) = specs.kernelspecs.get(name) {
            metadata = json!({"kernelspec": {"display_name": spec.spec.display_name,
                "language": spec.spec.language, "name": name}});
        } else if kernel.is_some() {
            return Err(err(format!("no kernel named {name}")));
        }
        let cells: Vec<Value> = title
            .map(|t| json!({"cell_type": "markdown", "id": new_id(), "metadata": {}, "source": format!("# {t}")}))
            .into_iter()
            .collect();
        let nb = json!({"cells": cells, "metadata": metadata, "nbformat": 4, "nbformat_minor": 5});
        // Its directories, as `mkdir -p`.
        let mut dir = String::new();
        for part in path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map_or(&[][..], |(_, d)| d)
        {
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(part);
            if self
                .state
                .contents
                .get(&dir, GetOptions::default())
                .await
                .is_err()
            {
                self.state
                    .contents
                    .save(&dir, SaveRequest::directory())
                    .await
                    .map_err(|e| err(e.to_string()))?;
            }
        }
        self.state
            .contents
            .save(path, SaveRequest::notebook(nb))
            .await
            .map_err(|e| err(e.to_string()))?;
        Ok(())
    }

    /// The kernel of the notebook's session, if it has one.
    fn running_kernel(&self, path: &str) -> Option<(String, String)> {
        self.state
            .sessions
            .list()
            .into_iter()
            .find(|s| s.path == path)
            .map(|s| (s.kernel.id, s.kernel.name))
    }

    /// The kernel of the notebook's session, started (with a session, so
    /// JupyterLab joins the same kernel) when there is none.
    async fn kernel_for(
        &self,
        path: &str,
        kernel: Option<&str>,
    ) -> Result<(String, String), McpError> {
        if let Some(s) = self
            .state
            .sessions
            .list()
            .into_iter()
            .find(|s| s.path == path)
        {
            return Ok((s.kernel.id, s.kernel.name));
        }
        let name = match kernel {
            Some(k) => Some(k.to_string()),
            None => self
                .notebook(path)
                .await
                .ok()
                .and_then(|nb| nb.metadata.get("kernelspec").cloned())
                .and_then(|k| k["name"].as_str().map(str::to_string)),
        };
        let s = self
            .state
            .sessions
            .create(SessionCreate {
                path: path.to_string(),
                name: Some(path.rsplit('/').next().unwrap_or(path).to_string()),
                type_: Some("notebook".into()),
                kernel: KernelRef { id: None, name },
            })
            .await
            .map_err(|e| err(e.message))?;
        Ok((s.kernel.id, s.kernel.name))
    }

    async fn insert(
        &self,
        path: &str,
        cell: Value,
        after: Option<String>,
        mark: Option<String>,
    ) -> Result<(), McpError> {
        let id = cell["id"].as_str().unwrap_or_default().to_string();
        let r = self
            .own_edit(path, vec![id], marks::Aspect::All, mark, move |room| {
                room.edit(|txn, roots: &Roots| {
                    let at = match &after {
                        None => roots.cells.len(txn),
                        Some(a) => match schema::cell_index(txn, roots, a) {
                            Some(i) => i + 1,
                            None => return Err(format!("no cell {a}")),
                        },
                    };
                    schema::insert_cell(txn, &roots.cells, at, cell);
                    Ok(())
                })
            })
            .await?;
        r.map_err(err)
    }

    async fn run_cells(
        &self,
        path: &str,
        kernel_id: &str,
        ids: Vec<String>,
        timeout: Duration,
    ) -> Result<CallToolResult, McpError> {
        let finished =
            crate::execute::run_and_wait(&self.state, path, kernel_id, ids.clone(), timeout)
                .await
                .map_err(err)?;
        let nb = self.notebook(path).await?;
        let mut shown = Vec::new();
        let mut out = String::new();
        if !finished {
            out.push_str("Timed out; the cell is still running. Outputs so far:\n\n");
        }
        for (index, cell) in nb.cells.iter().enumerate() {
            if cell.id().is_some_and(|id| ids.iter().any(|i| i == id)) {
                out.push_str(&self.render_cell(index, cell, true));
                out.push('\n');
                if self.opts.images {
                    if let Cell::Code(c) = cell {
                        shown.extend(c.outputs.iter().cloned());
                    }
                }
            }
        }
        let mut blocks = images_off_runtime(shown).await;
        blocks.insert(0, ContentBlock::text(out));
        Ok(CallToolResult::success(blocks))
    }

    async fn scratch(
        &self,
        kernel_id: &str,
        source: String,
        timeout: Duration,
    ) -> Result<CallToolResult, McpError> {
        self.state
            .kernels
            .wait_started(kernel_id)
            .await
            .map_err(|e| err(e.message))?;
        let kernel = self
            .state
            .kernels
            .get(kernel_id)
            .map_err(|e| err(e.message))?;
        kernel.touch();
        let cell = ganymede_nbformat::CodeCell {
            source: source.into(),
            ..Default::default()
        };
        let opts = ganymede_exec::Options {
            timeout: Some(timeout),
            allow_errors: true,
            record_timing: false,
            store_widget_state: false,
            coalesce_streams: true,
            // Scratch runs don't advance the kernel's execution count.
            store_history: false,
            ..Default::default()
        };
        // Whether the kernel took the request up: it sends `busy` for the
        // request, and `execute_input` or an output, only once it runs it.
        let mut started = Started(false);
        let mut cells = vec![Cell::Code(cell)];
        let r = ganymede_exec::NotebookRun::new(opts)
            .execute_cell_with(&kernel.session(), &mut cells, 0, &mut started)
            .await;
        let started = started.0;
        let Some(Cell::Code(cell)) = cells.pop() else {
            unreachable!("one code cell went in")
        };
        let mut out = String::new();
        if let Err(e) = &r {
            out.push_str(&format!("{e}\n"));
        }
        // Stop a timed-out scratch run rather than leave the shared kernel
        // busy: its outputs go nowhere, and later runs would queue behind
        // it. Only when the kernel is running it: still queued, it waits
        // behind another run (a recorded cell that timed out, a cell a
        // person ran), which an interrupt would stop instead.
        if let Err(ganymede_exec::Error::CellTimeout { .. }) = r {
            if !started {
                out.push_str(
                    "The kernel hadn't started it: it is busy with another run, and this \
                     code runs when that finishes. Interrupt the kernel to stop that run.\n",
                );
            } else {
                match self.state.kernels.interrupt(kernel_id).await {
                    Ok(_) => out.push_str("Interrupted the kernel.\n"),
                    Err(e) => out.push_str(&format!(
                        "Interrupting the kernel failed ({}); it may still be running the code.\n",
                        e.message
                    )),
                }
            }
        }
        kernel.touch();
        out.push_str(&self.render_outputs(&cell.outputs));
        if out.is_empty() {
            out.push_str("(no output)");
        }
        let mut blocks = vec![ContentBlock::text(out)];
        if self.opts.images {
            blocks.extend(images_off_runtime(cell.outputs.clone()).await);
        }
        Ok(CallToolResult::success(blocks))
    }

    fn render(&self, nb: &Notebook, outputs: bool) -> String {
        let mut out = String::new();
        for (i, cell) in nb.cells.iter().enumerate() {
            out.push_str(&self.render_cell(i, cell, outputs));
            out.push('\n');
        }
        if out.is_empty() {
            out.push_str("(empty notebook)\n");
        }
        out
    }

    /// What [`Notebooks::read_since`] found: the changed cells as
    /// `@@cell` blocks whose head says how they changed, and an
    /// `@@deleted` line per deleted cell.
    fn render_changes(&self, nb: &Notebook, c: &marks::Changes, token: &str) -> String {
        if c.is_empty() {
            return format!("No changes by others since that read.\n@@read {token}\n");
        }
        let mut out = format!(
            "Changed by others since that read: {} added, {} edited, {} moved, {} deleted.\n\n",
            c.added.len(),
            c.edited.len(),
            c.moved.len(),
            c.deleted.len()
        );
        for (index, cell) in nb.cells.iter().enumerate() {
            let Some(id) = cell.id() else { continue };
            let has = |v: &[String]| v.iter().any(|i| i == id);
            let mut how = Vec::new();
            if has(&c.added) {
                how.push("added");
            }
            if has(&c.edited) {
                how.push("edited");
            }
            if has(&c.moved) {
                how.push("moved");
            }
            if !how.is_empty() {
                out.push_str(&self.render_cell_as(index, cell, true, Some(&how)));
                out.push('\n');
            }
        }
        for id in &c.deleted {
            out.push_str(&format!("@@deleted {}\n", json!({"id": id})));
        }
        out.push_str(&format!("@@read {token}\n"));
        out
    }

    fn render_cell(&self, index: usize, cell: &Cell, outputs: bool) -> String {
        self.render_cell_as(index, cell, outputs, None)
    }

    fn render_cell_as(
        &self,
        index: usize,
        cell: &Cell,
        outputs: bool,
        change: Option<&[&str]>,
    ) -> String {
        let (kind, count, source, outs) = match cell {
            Cell::Code(c) => (
                "code",
                c.execution_count,
                c.source.as_str(),
                Some(&c.outputs),
            ),
            Cell::Markdown(c) => ("markdown", None, c.source.as_str(), None),
            Cell::Raw(c) => ("raw", None, c.source.as_str(), None),
            _ => ("unknown", None, "", None),
        };
        let mut head = json!({"index": index, "id": cell.id(), "cell_type": kind});
        if let Some(n) = count {
            head["execution_count"] = json!(n);
        }
        if let Some(how) = change {
            head["change"] = json!(how);
        }
        let mut s = format!("@@cell {head}\n");
        if kind == "code" {
            s.push_str(&format!("```\n{source}\n```\n"));
        } else {
            s.push_str(source);
            s.push('\n');
        }
        if let (true, Some(outs)) = (outputs, outs) {
            if !outs.is_empty() {
                s.push('\n');
                s.push_str(&self.render_outputs(outs));
            }
        }
        s
    }

    fn render_outputs(&self, outputs: &[Output]) -> String {
        let mut s = String::new();
        for o in outputs {
            let v = serde_json::to_value(o).unwrap_or_default();
            let kind = v["output_type"].as_str().unwrap_or_default();
            let mut head = json!({"output_type": kind});
            let body = match kind {
                "stream" => {
                    head["name"] = v["name"].clone();
                    v["text"].as_str().unwrap_or_default().to_string()
                }
                "error" => {
                    head["ename"] = v["ename"].clone();
                    head["evalue"] = v["evalue"].clone();
                    let frames: Vec<String> = v["traceback"]
                        .as_array()
                        .map(|t| {
                            t.iter()
                                .filter_map(Value::as_str)
                                .map(ganymede_nbformat::strip_ansi)
                                .collect()
                        })
                        .unwrap_or_default();
                    let skip = frames.len().saturating_sub(TRACEBACK_FRAMES);
                    let mut b = String::new();
                    if skip > 0 {
                        b.push_str(&format!("[{skip} earlier frames]\n"));
                    }
                    b.push_str(&frames[skip..].join("\n"));
                    b
                }
                _ => {
                    if let Some(n) = v.get("execution_count").filter(|n| n.is_number()) {
                        head["execution_count"] = n.clone();
                    }
                    let (mime, body) = best_text(&v["data"]);
                    if let Some(m) = mime {
                        head["mime"] = json!(m);
                    }
                    body
                }
            };
            let body = if body.chars().count() > INLINE_LIMIT {
                match self.spill(&body) {
                    Ok(p) => {
                        head["path"] = json!(p.to_string_lossy());
                        let cut: String = body.chars().take(500).collect();
                        format!(
                            "{cut}\n[{} characters; the rest is in the file]",
                            body.chars().count()
                        )
                    }
                    Err(_) => body.chars().take(INLINE_LIMIT).collect(),
                }
            } else {
                body
            };
            s.push_str(&format!(
                "@@output {head}\n{}\n",
                body.trim_end_matches('\n')
            ));
        }
        s
    }

    /// Write a long output to a file named by its sha256, in a directory
    /// only this user can read (outputs may print secrets): by default
    /// under the Jupyter runtime dir, not the shared temp dir. The file is
    /// always written, never trusted because it exists.
    fn spill(&self, body: &str) -> std::io::Result<PathBuf> {
        let dir = self.opts.outputs_dir.clone().unwrap_or_else(|| {
            ganymede_kernel_host::kernelspec::jupyter_runtime_dir().join("ganymede-mcp-outputs")
        });
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir)?;
        let name = hex::encode(sha2::Sha256::digest(body.as_bytes()));
        let path = dir.join(format!("{name}.txt"));
        std::fs::write(&path, body)?;
        Ok(path)
    }
}

/// Markdown, then plain text, then HTML with tags stripped.
fn best_text(data: &Value) -> (Option<&'static str>, String) {
    let get = |m: &str| data.get(m).and_then(Value::as_str).map(str::to_string);
    if let Some(t) = get("text/markdown") {
        return (Some("text/markdown"), t);
    }
    if let Some(t) = get("text/plain") {
        return (Some("text/plain"), t);
    }
    if let Some(t) = get("text/html") {
        return (Some("text/html"), strip_html(&t));
    }
    let kinds: Vec<&str> = data
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    (None, format!("[{}]", kinds.join(", ")))
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Whether a scratch run's request was taken up by the kernel (#340).
struct Started(bool);

impl ganymede_exec::NotebookHandler for Started {
    fn on_event(&mut self, e: ganymede_exec::Event<'_>) {
        if let ganymede_exec::Event::Output { .. } = e {
            self.0 = true;
        }
    }
    fn on_busy(&mut self, _index: usize) {
        self.0 = true;
    }
}

/// [`images`] on a blocking thread: drawing an SVG loads the system fonts
/// the first time and rasterizes up to 1,600 px square, which would hold
/// up the runtime's other work (under `serve --mcp`, the whole server).
async fn images_off_runtime(outputs: Vec<Output>) -> Vec<ContentBlock> {
    if outputs.is_empty() {
        return Vec::new();
    }
    tokio::task::spawn_blocking(move || images(&outputs))
        .await
        .unwrap_or_default()
}

/// The image blocks for `outputs`: raster images as they are, and an SVG
/// as a PNG when the output has no raster image of its own. An SVG that
/// can't be drawn adds nothing; the text reply still names it.
fn images(outputs: &[Output]) -> Vec<ContentBlock> {
    let b64 = &base64::engine::general_purpose::STANDARD;
    let mut blocks = Vec::new();
    for o in outputs {
        let v = serde_json::to_value(o).unwrap_or_default();
        let before = blocks.len();
        for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            if let Some(data) = v["data"][mime].as_str() {
                let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
                if b64.decode(&clean).is_ok() {
                    blocks.push(ContentBlock::image(clean, mime));
                }
            }
        }
        if blocks.len() == before {
            if let Some(png) = svg_source(&v["data"]["image/svg+xml"]).and_then(|s| svg::to_png(&s))
            {
                blocks.push(ContentBlock::image(b64.encode(png), "image/png"));
            }
        }
    }
    blocks
}

/// An SVG mime bundle value: one string, or lines as nbformat stores them.
fn svg_source(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(lines) => lines.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helpers() {
        assert_eq!(strip_html("<b>a &amp; b</b><br/>c"), "a & bc");
        let (m, t) = best_text(&json!({"text/html": "<p>x</p>", "text/plain": "y"}));
        assert_eq!((m, t.as_str()), (Some("text/plain"), "y"));
    }

    #[cfg(feature = "svg")]
    #[test]
    fn svg_outputs_go_to_the_agent_as_png() {
        let output = |data: Value| -> Output {
            serde_json::from_value(json!({
                "output_type": "display_data", "metadata": {}, "data": data
            }))
            .unwrap()
        };
        let svg = [
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"20\">\n",
            "<rect width=\"40\" height=\"20\" fill=\"red\"/></svg>",
        ];
        // Lines, as nbformat stores them; and with a PNG of its own, the
        // output's PNG goes instead.
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG");
        let outputs = [
            output(json!({"image/svg+xml": svg, "text/plain": "<Figure>"})),
            output(json!({"image/svg+xml": svg.concat(), "image/png": png})),
            output(json!({"image/svg+xml": "<svg", "text/plain": "broken"})),
        ];
        let blocks = images(&outputs);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        let image = |b: &ContentBlock| {
            let v = serde_json::to_value(b).unwrap();
            (
                v["mimeType"].as_str().unwrap().to_string(),
                v["data"].as_str().unwrap().to_string(),
            )
        };
        let (mime, data) = image(&blocks[0]);
        assert_eq!(mime, "image/png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap();
        assert_eq!(&bytes[..4], b"\x89PNG");
        assert!(bytes.len() > 8, "a whole PNG, not the second output's");
        assert_eq!(image(&blocks[1]), ("image/png".to_string(), png));
    }
}
