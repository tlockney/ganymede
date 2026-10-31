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
pub mod server_info;
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
    /// Origins allowed besides the request's own host: for websockets,
    /// cookie-authenticated requests, and CORS (`Access-Control-Allow-*`
    /// headers and preflight answers).
    pub allow_origins: Vec<String>,
    /// Send `Access-Control-Allow-Credentials: true` to listed origins, so
    /// a page on one of them can use the login cookie. Off by default, as
    /// `jupyter_server`'s `allow_credentials`; cross-origin clients then
    /// authenticate with the token.
    pub allow_credentials: bool,
    /// Skip the `Host` check. Off by default when bound to loopback, so a
    /// DNS-rebinding page cannot reach the server through its own name.
    pub allow_remote_access: bool,
    /// Names, besides loopback addresses, accepted in `Host` when
    /// `allow_remote_access` is off.
    pub local_hostnames: Vec<String>,
    /// Believe `X-Forwarded-Proto` (and `X-Scheme`) from a reverse proxy,
    /// so login cookies get `Secure` behind a TLS terminator.
    /// `jupyter_server`'s `trust_xheaders`; off by default, since any
    /// client can send the header.
    pub trust_xheaders: bool,
    /// Largest request body accepted, in bytes; larger ones get 413.
    pub max_body_size: usize,
    /// URL prefix the routes are nested under, with a trailing `/`.
    pub base_url: String,
    /// Jupyter config dir: `nbconfig/` for `/api/config`, and JupyterLab's
    /// `lab/user-settings` and `lab/workspaces`.
    pub config_dir: PathBuf,
    /// JupyterLab's application, when `--static-dir` points at its bundle.
    pub lab: Option<Arc<lab::Lab>>,
    /// Any other `--static-dir`, served at `/` behind the routes.
    pub static_dir: Option<PathBuf>,
    /// `/api/terminals`; `None` when `terminals_enabled` is off or the
    /// platform cannot start processes (iOS).
    pub terminals: Option<Arc<ganymede_terminals::TerminalManager>>,
}

/// The API router, login pages, and JupyterLab or generic static assets
/// when configured, before the base URL prefix.
pub fn build_router(state: AppState) -> axum::Router {
    let mut app = routes::router(state.clone()).merge(auth::router(state.clone()));
    match (state.lab.clone(), &state.static_dir) {
        (Some(lab), _) => app = app.merge(lab::router(state.clone(), &lab)),
        (None, Some(dir)) => {
            let files =
                tower_http::services::ServeDir::new(dir).append_index_html_on_directories(true);
            app = app.fallback_service(axum::Router::new().fallback_service(files).layer(
                axum::middleware::from_fn_with_state(state.clone(), auth::static_page),
            ))
        }
        (None, None) => {}
    }
    app = app
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::strip_token,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(state.max_body_size));
    if !state.allow_origins.is_empty() {
        app = app.layer(cors(state.allow_origins.clone(), state.allow_credentials));
    }
    app.layer(axum::middleware::from_fn_with_state(
        state,
        auth::check_host,
    ))
}

/// CORS for `allow_origins`, as `jupyter_server` sends it: the allowed
/// origin echoed in `Access-Control-Allow-Origin`, and preflight requests
/// answered before auth with the methods and headers its API handlers
/// accept. With `allow_credentials`, also
/// `Access-Control-Allow-Credentials: true`, except when `*` is listed:
/// `jupyter_server` then sends `Access-Control-Allow-Origin: *`, which
/// browsers never pair with credentials, so no page gets the cookie that
/// way. Here every origin is echoed, so credentials are left off instead.
fn cors(allow_origins: Vec<String>, allow_credentials: bool) -> tower_http::cors::CorsLayer {
    use axum::http::{header, HeaderName, Method};
    use tower_http::cors::{AllowOrigin, CorsLayer};
    let credentials = allow_credentials && !allow_origins.iter().any(|o| o == "*");
    if allow_credentials && !credentials {
        tracing::warn!("allow_credentials is ignored while allow_origins contains \"*\"");
    }
    CorsLayer::new()
        .allow_credentials(credentials)
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin
                .to_str()
                .is_ok_and(|o| routes::origin_listed(&allow_origins, o))
        }))
        .allow_methods([
            Method::GET,
            Method::PUT,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::ACCEPT,
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::IF_MATCH,
            HeaderName::from_static("x-xsrftoken"),
        ])
        .expose_headers([header::ETAG])
}
