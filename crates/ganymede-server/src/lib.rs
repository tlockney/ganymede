//! HTTP server: `jupyter_server`'s REST and websocket API over
//! [`ganymede_api`] and [`ganymede_contents`], with token and cookie auth and
//! static assets.
//!
//! Every handler is a thin mapping from a route to an API call; the logic
//! lives in the API crates so an in-app bridge can skip HTTP entirely.

#![recursion_limit = "256"]

pub mod auth;
pub mod collab;
pub mod config;
pub mod execute;
pub mod host;
pub mod lab;
pub mod mcp;
pub mod metrics;
pub(crate) mod room_lock;
pub mod routes;
pub mod run;
pub mod serve;
pub mod server_info;
pub mod static_headers;
mod svg;
pub mod watch;
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
    /// Extra response headers for files from `static_dir`, by path.
    pub static_headers: Arc<[static_headers::StaticHeaders]>,
    /// The `frame-ancestors` directive added to every response's
    /// `Content-Security-Policy`; see [`frame_ancestors_directive`].
    pub frame_ancestors: axum::http::HeaderValue,
    /// Document rooms behind `/api/collaboration`.
    pub collab: Arc<collab::Collab>,
    /// `/api/terminals`; `None` when `terminals_enabled` is off or the
    /// platform cannot start processes (iOS).
    pub terminals: Option<Arc<ganymede_terminals::TerminalManager>>,
    /// `/lsp/*`; `None` when no language server is configured or the
    /// platform cannot start processes (iOS).
    pub lsp: Option<Arc<ganymede_lsp::LspManager>>,
    /// `/mcp`; `None` unless `mcp_enabled`.
    pub mcp: Option<Arc<mcp::McpState>>,
    /// `/api/metrics/v1`: memory and CPU of the server and its kernels.
    pub metrics: Arc<metrics::Metrics>,
    /// `[host]` from the config: the host API's default grants, shown by
    /// `/api/host/grants`.
    pub host: Arc<host::HostConfig>,
}

/// The API router, login pages, and JupyterLab or generic static assets
/// when configured, before the base URL prefix.
pub fn build_router(state: AppState) -> axum::Router {
    let mut app = routes::router(state.clone()).merge(auth::router(state.clone()));
    if let Some(m) = &state.mcp {
        app = app.merge(mcp::router(state.clone(), m));
    }
    match (state.lab.clone(), &state.static_dir) {
        (Some(lab), _) => app = app.merge(lab::router(state.clone(), &lab)),
        (None, Some(dir)) => {
            let files =
                tower_http::services::ServeDir::new(dir).append_index_html_on_directories(true);
            app = app.fallback_service(
                axum::Router::new()
                    .fallback_service(files)
                    .layer(axum::middleware::from_fn_with_state(
                        state.clone(),
                        auth::static_page,
                    ))
                    .layer(axum::middleware::from_fn_with_state(
                        state.static_headers.clone(),
                        static_headers::add,
                    )),
            )
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
    let directive = state.frame_ancestors.clone();
    app.layer(axum::middleware::from_fn_with_state(
        state,
        auth::check_host,
    ))
    .layer(axum::middleware::from_fn_with_state(
        directive,
        frame_ancestors,
    ))
}

/// The default `frame_ancestors` sources, as `jupyter_server`'s handlers
/// send them: only pages of this server may frame its pages, so another
/// site can't overlay `/lab` and steer clicks.
pub const DEFAULT_FRAME_ANCESTORS: &str = "'self'";

/// `frame-ancestors <sources>` as a header value, for the config's
/// `frame_ancestors` (such as `'self' https://portal.example.com`). Fails
/// on a value that isn't valid HTTP or would end the directive.
pub fn frame_ancestors_directive(sources: &str) -> anyhow::Result<axum::http::HeaderValue> {
    let sources = sources.trim();
    anyhow::ensure!(
        !sources.is_empty() && !sources.contains([';', ',']),
        "frame_ancestors: {sources:?} must be a space-separated list of CSP sources"
    );
    axum::http::HeaderValue::try_from(format!("frame-ancestors {sources}"))
        .map_err(|e| anyhow::anyhow!("frame_ancestors: {e}"))
}

/// Middleware that adds the `frame-ancestors` directive in its state to
/// the response's `Content-Security-Policy`: as the whole policy when
/// there is none, appended to one a handler or `static_headers` set, and
/// not at all when that policy already has a `frame-ancestors` directive,
/// so a configured one wins. Applying it twice changes nothing.
pub async fn frame_ancestors(
    axum::extract::State(directive): axum::extract::State<axum::http::HeaderValue>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header::CONTENT_SECURITY_POLICY;
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    let policy = match headers.get(CONTENT_SECURITY_POLICY) {
        None => Some(directive),
        Some(v) => {
            let has = v.to_str().is_ok_and(|s| {
                s.split(';').any(|d| {
                    d.split_ascii_whitespace()
                        .next()
                        .is_some_and(|n| n.eq_ignore_ascii_case("frame-ancestors"))
                })
            });
            if has {
                None
            } else {
                let mut joined = v.as_bytes().to_vec();
                joined.extend_from_slice(b"; ");
                joined.extend_from_slice(directive.as_bytes());
                axum::http::HeaderValue::from_bytes(&joined).ok()
            }
        }
    };
    if let Some(p) = policy {
        headers.insert(CONTENT_SECURITY_POLICY, p);
    }
    res
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
