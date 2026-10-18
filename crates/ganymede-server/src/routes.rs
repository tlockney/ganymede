use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware;
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
            .map_err(|e| HttpError(e.status().as_u16(), e.body_text()))?;
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(LenientJson(T::default()));
        }
        serde_json::from_slice(&bytes)
            .map(LenientJson)
            .map_err(|e| HttpError(400, format!("invalid JSON body: {e}")))
    }
}

/// The `jupyter_server` version whose REST API this server matches.
/// Reported by `GET /api`, which clients use to check what they talk to.
pub const JUPYTER_SERVER_VERSION: &str = "2.21.1";

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/me", get(me))
        .route("/api/config/{section}", get(get_config).patch(patch_config))
        .route("/api/nbconvert", get(nbconvert))
        .route("/api/events/subscribe", get(crate::ws::events))
        .route("/kernelspecs/{name}/{*file}", get(kernelspec_resource))
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
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::api,
        ))
        .route("/api", get(api_version));
    api.with_state(state)
}

// ----- status, identity, config ---------------------------------------------

async fn api_version() -> Json<Value> {
    Json(json!({ "version": JUPYTER_SERVER_VERSION }))
}

#[derive(Deserialize, Default)]
struct MeQuery {
    permissions: Option<String>,
}

/// One identity for whoever holds the token. `jupyter_server` invents an
/// anonymous name per browser; this server has no users to tell apart.
/// `?permissions={"resource": ["action", ...]}` is answered with the
/// actions allowed, which is all of them: the token grants everything.
async fn me(Query(q): Query<MeQuery>) -> HttpResult<Json<Value>> {
    let permissions = match q.permissions.as_deref() {
        None | Some("") => json!({}),
        Some(p) => {
            let bad = || {
                HttpError(
                    400,
                    "permissions should be a JSON dict of {\"resource\": [\"action\",]}, got "
                        .to_string()
                        + p,
                )
            };
            let v: Value = serde_json::from_str(p).map_err(|_| bad())?;
            let valid = v.as_object().is_some_and(|m| {
                m.values()
                    .all(|a| a.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
            });
            if !valid {
                return Err(bad());
            }
            v
        }
    };
    Ok(Json(json!({
        "identity": {
            "username": "anonymous",
            "name": "Anonymous",
            "display_name": "Anonymous",
            "initials": "A",
            "avatar_url": null,
            "color": null,
        },
        "permissions": permissions,
    })))
}

/// Frontend config sections, stored as `nbconfig/{section}.json` in the
/// Jupyter config dir, as `jupyter_server` does.
fn config_path(s: &AppState, section: &str) -> HttpResult<std::path::PathBuf> {
    if section.is_empty()
        || !section
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || section.starts_with('.')
    {
        return Err(HttpError(400, format!("bad config section {section:?}")));
    }
    Ok(s.config_dir
        .join("nbconfig")
        .join(format!("{section}.json")))
}

fn read_config(path: &std::path::Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

async fn get_config(
    State(s): State<AppState>,
    Path(section): Path<String>,
) -> HttpResult<Json<Value>> {
    Ok(Json(read_config(&config_path(&s, &section)?)))
}

/// Recursive merge; `null` deletes a key.
async fn patch_config(
    State(s): State<AppState>,
    Path(section): Path<String>,
    LenientJson(update): LenientJson<Value>,
) -> HttpResult<Json<Value>> {
    let path = config_path(&s, &section)?;
    let mut current = read_config(&path);
    merge(&mut current, update);
    // The OS path goes to the log only; clients see the section name.
    let io = |e: std::io::Error| {
        tracing::warn!("writing {}: {e}", path.display());
        HttpError(500, format!("cannot write config section {section}: {e}"))
    };
    std::fs::create_dir_all(path.parent().unwrap()).map_err(io)?;
    std::fs::write(&path, serde_json::to_vec_pretty(&current).unwrap()).map_err(io)?;
    Ok(Json(current))
}

fn merge(target: &mut Value, update: Value) {
    let (Value::Object(t), Value::Object(u)) = (&mut *target, &update) else {
        if !update.is_null() {
            *target = update;
        }
        return;
    };
    for (k, v) in u.clone() {
        if v.is_null() {
            t.remove(&k);
        } else if v.is_object() && t.get(&k).map(Value::is_object).unwrap_or(false) {
            merge(t.get_mut(&k).unwrap(), v);
        } else {
            t.insert(k, v);
        }
    }
}

/// Export formats. nbconvert is Python, so there are none.
async fn nbconvert() -> Json<Value> {
    Json(json!({}))
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

/// `kernelspecs/{name}/{file}`: logos and other files from the
/// kernelspec's directory.
async fn kernelspec_resource(
    State(s): State<AppState>,
    Path((name, file)): Path<(String, String)>,
) -> HttpResult<Response> {
    if file
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        || file.contains('\\')
    {
        return Err(HttpError(404, "not found".into()));
    }
    let dir = s.kernels.kernelspec_resource_dir(&name)?;
    let path = dir.join(&file);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| HttpError(404, format!("no resource {file} in kernelspec {name}")))?;
    let mime = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
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
    let location = format!("{}api/kernels/{}", s.base_url, url_escape(&model.id));
    Ok(created(location, model))
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
    let location = format!("{}api/sessions/{}", s.base_url, url_escape(&model.id));
    Ok(created(location, model))
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
    hash: Option<String>,
}

/// `/api/contents/a/b/checkpoints` and `/checkpoints/{id}` share the
/// wildcard route with plain paths; split them here, as `jupyter_server`'s
/// route order does.
enum ContentsTarget {
    Path(String),
    Checkpoints(String),
    Checkpoint(String, String),
    Trust(String),
}

fn target(path: &str) -> ContentsTarget {
    let path = ganymede_contents::normalize_api_path(path);
    if let Some(p) = path.strip_suffix("/trust") {
        return ContentsTarget::Trust(p.to_string());
    }
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
        ContentsTarget::Trust(_) => Err(method_not_allowed()),
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
                        hash: q.hash.as_deref().is_some_and(|h| h != "0"),
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
    let p = match target(&path) {
        ContentsTarget::Path(p) => p,
        ContentsTarget::Trust(_) => return Err(method_not_allowed()),
        _ => return Err(HttpError(400, "cannot PUT a checkpoint".into())),
    };
    let req =
        req.ok_or_else(|| HttpError(400, "PUT needs a body with type, format, content".into()))?;
    let existed = s.contents.get(&p, GetOptions::default()).await.is_ok();
    let model = s.contents.save(&p, req).await?;
    if existed {
        Ok(Json(model).into_response())
    } else {
        Ok(created(contents_location(&s, &model.path), model))
    }
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
    let p = match target(&path) {
        ContentsTarget::Path(p) => p,
        ContentsTarget::Trust(_) => return Err(method_not_allowed()),
        _ => return Err(HttpError(400, "cannot rename a checkpoint".into())),
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
        ContentsTarget::Checkpoints(_) | ContentsTarget::Trust(_) => {
            return Err(method_not_allowed())
        }
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
            let location = format!(
                "{}/checkpoints/{}",
                contents_location(&s, &p),
                url_escape(&cp.id)
            );
            Ok(created(location, cp))
        }
        ContentsTarget::Checkpoint(p, id) => {
            s.contents.restore_checkpoint(&p, &id).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        // jupyter_server's TrustNotebooksHandler: 201 with an empty body.
        ContentsTarget::Trust(p) => {
            s.contents.trust(&p).await?;
            Ok(StatusCode::CREATED.into_response())
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
            Ok(created(contents_location(&s, &model.path), model))
        }
    }
}

/// 201 with a `Location` header, as `jupyter_server` answers a create.
fn created(location: String, body: impl serde::Serialize) -> Response {
    let mut resp = (StatusCode::CREATED, Json(body)).into_response();
    if let Ok(v) = header::HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

/// The URL of a contents path, each segment percent-encoded.
fn contents_location(s: &AppState, path: &str) -> String {
    format!("{}api/contents/{}", s.base_url, url_escape(path))
}

/// `jupyter_server.utils.url_escape`: percent-encode each `/`-separated
/// segment, as Python's `quote` does.
fn url_escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/_.-~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn method_not_allowed() -> HttpError {
    HttpError(405, "method not allowed".into())
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
    origin_host == host || origin_listed(&state.allow_origins, origin)
}

/// Whether `origin` is in `allow_origins`: `*`, the full origin, or its
/// host and port.
pub(crate) fn origin_listed(allow_origins: &[String], origin: &str) -> bool {
    let origin_host = origin.split("://").nth(1).unwrap_or(origin);
    allow_origins
        .iter()
        .any(|o| o == "*" || o == origin || o == origin_host)
}
