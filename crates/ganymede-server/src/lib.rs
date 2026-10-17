//! HTTP server: `jupyter_server`'s REST and websocket API over
//! [`ganymede_api`] and [`ganymede_contents`], with token auth and static assets.
//!
//! Every handler is a thin mapping from a route to an API call; the logic
//! lives in the API crates so an in-app bridge can skip HTTP entirely.

#![recursion_limit = "256"]

pub mod config;
pub mod lab;
pub mod routes;
pub mod serve;
pub mod ws;

use ganymede_api::{KernelManager, SessionManager};
use ganymede_contents::Contents;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub kernels: Arc<KernelManager>,
    pub sessions: Arc<SessionManager>,
    pub contents: Arc<dyn Contents>,
    /// `None` disables auth. The server generates one at startup when
    /// nothing is configured.
    pub token: Option<String>,
    /// Origins allowed to open websockets, beyond the request's own host.
    pub allow_origins: Vec<String>,
    /// URL prefix the routes are nested under, with a trailing `/`.
    pub base_url: String,
    /// Jupyter config dir: `nbconfig/` for `/api/config`, and JupyterLab's
    /// `lab/user-settings` and `lab/workspaces`.
    pub config_dir: PathBuf,
    /// JupyterLab's application, when `--static-dir` points at its bundle.
    pub lab: Option<Arc<lab::Lab>>,
}

/// The API router, plus JupyterLab when configured, before generic static
/// assets and the base URL prefix.
pub fn build_router(state: AppState) -> axum::Router {
    let api = routes::router(state.clone());
    match state.lab.clone() {
        Some(lab) => api.merge(lab::router(state, &lab)),
        None => api,
    }
}
