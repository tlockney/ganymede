//! HTTP server: `jupyter_server`'s REST and websocket API over
//! [`ganymede_api`] and [`ganymede_contents`], with token and cookie auth and
//! static assets.
//!
//! Every handler is a thin mapping from a route to an API call; the logic
//! lives in the API crates so an in-app bridge can skip HTTP entirely.

#![recursion_limit = "256"]

pub mod auth;
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
    /// Key for signing login cookies; see [`auth::load_cookie_secret`].
    pub cookie_secret: Arc<[u8]>,
    /// Origins allowed to open websockets, beyond the request's own host.
    pub allow_origins: Vec<String>,
    /// Skip the `Host` check. Off by default when bound to loopback, so a
    /// DNS-rebinding page cannot reach the server through its own name.
    pub allow_remote_access: bool,
    /// Names, besides loopback addresses, accepted in `Host` when
    /// `allow_remote_access` is off.
    pub local_hostnames: Vec<String>,
    /// URL prefix the routes are nested under, with a trailing `/`.
    pub base_url: String,
    /// Jupyter config dir: `nbconfig/` for `/api/config`, and JupyterLab's
    /// `lab/user-settings` and `lab/workspaces`.
    pub config_dir: PathBuf,
    /// JupyterLab's application, when `--static-dir` points at its bundle.
    pub lab: Option<Arc<lab::Lab>>,
    /// Any other `--static-dir`, served at `/` behind the routes.
    pub static_dir: Option<PathBuf>,
}

/// The API router, login pages, and JupyterLab or generic static assets
/// when configured, before the base URL prefix.
pub fn build_router(state: AppState) -> axum::Router {
    let mut app = routes::router(state.clone()).merge(auth::router(state.clone()));
    match (state.lab.clone(), &state.static_dir) {
        (Some(lab), _) => app = app.merge(lab::router(state.clone(), &lab)),
        (None, Some(dir)) => {
            app = app.fallback_service(
                tower_http::services::ServeDir::new(dir).append_index_html_on_directories(true),
            )
        }
        (None, None) => {}
    }
    app.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::strip_token,
    ))
    .layer(axum::middleware::from_fn_with_state(
        state,
        auth::check_host,
    ))
}
