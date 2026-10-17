use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ganymede_api::{ApiError, KernelStart, SessionCreate, SessionPatch, StatusModel};
use ganymede_contents::{ContentType, Format, GetOptions, SaveRequest};
use serde::Deserialize;
use serde_json::{json, Value};

/// Errors as `jupyter_server` sends them: `{"message": ..., "reason": null}`.
pub struct HttpError(pub u16, pub String);

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({"message": self.1, "reason": null}))).into_response()
    }
}

impl From<ApiError> for HttpError {
    fn from(e: ApiError) -> Self {
        HttpError(e.status, e.message)
    }
}

impl From<ganymede_contents::Error> for HttpError {
    fn from(e: ganymede_contents::Error) -> Self {
        HttpError(e.status(), e.to_string())
    }
}

type HttpResult<T> = std::result::Result<T, HttpError>;

/// A JSON body parsed regardless of `Content-Type`, as `jupyter_server`
/// does. JupyterLab's client omits the header on some requests, which
/// axum's `Json` extractor rejects with 415. An empty body is `T::default()`.
pub struct LenientJson<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for LenientJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Default,
{
    type Rejection = HttpError;

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| HttpError(400, e.to_string()))?;
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(LenientJson(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(LenientJson)
            .map_err(|e| HttpError(400, format!("invalid JSON body: {e}")))
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/kernelspecs", get(kernelspecs))
        .route("/api/kernelspecs/{name}", get(kernelspec))
        .route("/api/kernels", get(list_kernels).post(start_kernel))
        .route("/api/kernels/{id}", get(get_kernel).delete(delete_kernel))
        .route("/api/kernels/{id}/interrupt", post(interrupt_kernel))
        .route("/api/kernels/{id}/restart", post(restart_kernel))
        .route("/api/kernels/{id}/channels", get(crate::ws::channels))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route(
            "/api/contents",
            get(get_contents_root).post(post_contents_root),
        )
        .route(
            "/api/contents/{*path}",
            get(get_contents)
                .put(put_contents)
                .patch(patch_contents)
                .delete(delete_contents)
                .post(post_contents),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    api.with_state(state)
}

// ----- auth -----------------------------------------------------------------

/// `Authorization: token <t>` or `?token=<t>`. Browsers cannot set headers
/// on a websocket upgrade, so the query form matters there.
async fn auth(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected) = &state.token else {
        return next.run(req).await;
    };
    let header_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("token ")
                .or_else(|| v.strip_prefix("Token "))
        })
        .map(|t| t.trim() == expected)
        .unwrap_or(false);
    let query_ok = req
        .uri()
        .query()
        .and_then(serde_urlencoded_token)
        .map(|t| t == *expected)
        .unwrap_or(false);
    if header_ok || query_ok {
        next.run(req).await
    } else {
        HttpError(403, "Forbidden: missing or invalid token".into()).into_response()
    }
}

fn serde_urlencoded_token(query: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == "token").then(|| v.to_string())
    })
}

// ----- status, kernelspecs --------------------------------------------------

async fn status(State(s): State<AppState>) -> Json<StatusModel> {
    Json(StatusModel {
        started: s.kernels.started.clone(),
        last_activity: s.kernels.last_activity(),
        connections: s.kernels.connections(),
        kernels: s.kernels.list().len(),
    })
}

async fn kernelspecs(
    State(s): State<AppState>,
) -> HttpResult<Json<ganymede_api::KernelspecsModel>> {
    Ok(Json(s.kernels.kernelspecs()?))
}

async fn kernelspec(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> HttpResult<Json<ganymede_api::KernelspecModel>> {
    Ok(Json(s.kernels.kernelspec(&name)?))
}

// ----- kernels ----------------------------------------------------------------

async fn list_kernels(State(s): State<AppState>) -> Json<Vec<ganymede_api::KernelModel>> {
    Json(s.kernels.list())
}

async fn start_kernel(
    State(s): State<AppState>,
    LenientJson(req): LenientJson<KernelStart>,
) -> HttpResult<Response> {
    let model = s
        .kernels
        .start(req.name.as_deref(), req.path.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(model)).into_response())
}

async fn get_kernel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<Json<ganymede_api::KernelModel>> {
    Ok(Json(s.kernels.model(&id)?))
}

async fn delete_kernel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<StatusCode> {
    s.kernels.shutdown(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn interrupt_kernel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<StatusCode> {
    s.kernels.interrupt(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restart_kernel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<Json<ganymede_api::KernelModel>> {
    Ok(Json(s.kernels.restart(&id).await?))
}

// ----- sessions ---------------------------------------------------------------

async fn list_sessions(State(s): State<AppState>) -> Json<Vec<ganymede_api::SessionModel>> {
    Json(s.sessions.list())
}

async fn create_session(
    State(s): State<AppState>,
    LenientJson(req): LenientJson<SessionCreate>,
) -> HttpResult<Response> {
    let model = s.sessions.create(req).await?;
    Ok((StatusCode::CREATED, Json(model)).into_response())
}

async fn get_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<Json<ganymede_api::SessionModel>> {
    Ok(Json(s.sessions.get(&id)?))
}

async fn patch_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
    LenientJson(patch): LenientJson<SessionPatch>,
) -> HttpResult<Json<ganymede_api::SessionModel>> {
    Ok(Json(s.sessions.update(&id, patch).await?))
}

async fn delete_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<StatusCode> {
    s.sessions.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- contents ---------------------------------------------------------------

#[derive(Deserialize, Default)]
struct ContentsQuery {
    content: Option<String>,
    #[serde(rename = "type")]
    type_: Option<ContentType>,
    format: Option<Format>,
}

/// `/api/contents/a/b/checkpoints` and `/checkpoints/{id}` share the
/// wildcard route with plain paths; split them here, as `jupyter_server`'s
/// route order does.
enum ContentsTarget {
    Path(String),
    Checkpoints(String),
    Checkpoint(String, String),
}

fn target(path: &str) -> ContentsTarget {
    let path = ganymede_contents::normalize_api_path(path);
    if let Some(p) = path.strip_suffix("/checkpoints") {
        return ContentsTarget::Checkpoints(p.to_string());
    }
    if let Some((p, id)) = path.rsplit_once("/checkpoints/") {
        return ContentsTarget::Checkpoint(p.to_string(), id.to_string());
    }
    ContentsTarget::Path(path)
}

async fn get_contents_root(
    state: State<AppState>,
    q: Query<ContentsQuery>,
) -> HttpResult<Json<Value>> {
    get_contents(state, Path(String::new()), q).await
}

async fn get_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<ContentsQuery>,
) -> HttpResult<Json<Value>> {
    match target(&path) {
        ContentsTarget::Checkpoints(p) => Ok(Json(
            serde_json::to_value(s.contents.list_checkpoints(&p).await?).unwrap(),
        )),
        ContentsTarget::Checkpoint(..) => Err(HttpError(404, "not found".into())),
        ContentsTarget::Path(p) => {
            let content = q.content.as_deref().map(|c| c != "0").unwrap_or(true);
            let model = s
                .contents
                .get(
                    &p,
                    GetOptions {
                        content,
                        type_hint: q.type_,
                        format: q.format,
                    },
                )
                .await?;
            Ok(Json(serde_json::to_value(model).unwrap()))
        }
    }
}

async fn put_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    LenientJson(req): LenientJson<Option<SaveRequest>>,
) -> HttpResult<Response> {
    let ContentsTarget::Path(p) = target(&path) else {
        return Err(HttpError(400, "cannot PUT a checkpoint".into()));
    };
    let req =
        req.ok_or_else(|| HttpError(400, "PUT needs a body with type, format, content".into()))?;
    let existed = s.contents.get(&p, GetOptions::default()).await.is_ok();
    let model = s.contents.save(&p, req).await?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(model)).into_response())
}

#[derive(Deserialize, Default)]
struct RenameBody {
    path: Option<String>,
}

async fn patch_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    LenientJson(body): LenientJson<RenameBody>,
) -> HttpResult<Json<ganymede_contents::ContentsModel>> {
    let ContentsTarget::Path(p) = target(&path) else {
        return Err(HttpError(400, "cannot rename a checkpoint".into()));
    };
    let new_path = body
        .path
        .ok_or_else(|| HttpError(400, "PATCH needs {\"path\": ...}".into()))?;
    Ok(Json(
        s.contents
            .rename(&p, &ganymede_contents::normalize_api_path(&new_path))
            .await?,
    ))
}

async fn delete_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
) -> HttpResult<StatusCode> {
    match target(&path) {
        ContentsTarget::Path(p) => s.contents.delete(&p).await?,
        ContentsTarget::Checkpoint(p, id) => s.contents.delete_checkpoint(&p, &id).await?,
        ContentsTarget::Checkpoints(_) => return Err(HttpError(405, "method not allowed".into())),
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
struct NewContent {
    #[serde(rename = "type")]
    type_: Option<ContentType>,
    ext: Option<String>,
    copy_from: Option<String>,
}

async fn post_contents_root(
    state: State<AppState>,
    body: LenientJson<NewContent>,
) -> HttpResult<Response> {
    post_contents(state, Path(String::new()), body).await
}

async fn post_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    LenientJson(body): LenientJson<NewContent>,
) -> HttpResult<Response> {
    match target(&path) {
        ContentsTarget::Checkpoints(p) => {
            let cp = s.contents.create_checkpoint(&p).await?;
            Ok((StatusCode::CREATED, Json(cp)).into_response())
        }
        ContentsTarget::Checkpoint(p, id) => {
            s.contents.restore_checkpoint(&p, &id).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        ContentsTarget::Path(dir) => {
            let model = if let Some(from) = body.copy_from {
                s.contents
                    .copy(&ganymede_contents::normalize_api_path(&from), Some(&dir))
                    .await?
            } else {
                s.contents
                    .new_untitled(
                        &dir,
                        body.type_.unwrap_or(ContentType::Notebook),
                        body.ext.as_deref(),
                    )
                    .await?
            };
            Ok((StatusCode::CREATED, Json(model)).into_response())
        }
    }
}

/// Used by the websocket handler for its origin check.
pub(crate) fn origin_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let origin_host = origin.split("://").nth(1).unwrap_or(origin);
    origin_host == host
        || state
            .allow_origins
            .iter()
            .any(|o| o == "*" || o == origin || o == origin_host)
}
