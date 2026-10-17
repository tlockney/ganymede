//! HTTP server: `jupyter_server`'s REST and websocket API over
//! [`jcore_api`] and [`jcore_contents`], with token auth and static assets.
//!
//! Every handler is a thin mapping from a route to an API call; the logic
//! lives in the API crates so an in-app bridge can skip HTTP entirely.

pub mod config;
pub mod routes;
pub mod ws;

use jcore_api::{KernelManager, SessionManager};
use jcore_contents::Contents;
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
}

/// The API router, before static assets and the base URL prefix.
pub fn build_router(state: AppState) -> axum::Router {
    routes::router(state)
}
