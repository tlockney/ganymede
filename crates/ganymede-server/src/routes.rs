use crate::AppState;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ganymede_api::{ApiError, KernelStart, SessionCreate, SessionPatch, StatusModel};
use ganymede_contents::{ContentType, Format, GetOptions, IfMatch, SaveRequest};
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
/// Like Python's `json`, a lone surrogate escape is accepted (as U+FFFD), so
/// a notebook carrying one can be saved.
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
        ganymede_contents::from_json_slice(&bytes)
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
        .route("/nbconvert/{format}/{*path}", get(nbconvert_file))
        .route("/api/events/subscribe", get(crate::ws::events))
        .route("/kernelspecs/{name}/{*file}", get(kernelspec_resource))
        .route("/files/{*path}", get(get_file))
        .route("/api/kernelspecs", get(kernelspecs))
        .route("/api/kernelspecs/{name}", get(kernelspec))
        .route("/api/kernels", get(list_kernels).post(start_kernel))
        .route("/api/kernels/{id}", get(get_kernel).delete(delete_kernel))
        .route("/api/kernels/{id}/interrupt", post(interrupt_kernel))
        .route("/api/kernels/{id}/restart", post(restart_kernel))
        .route("/api/kernels/{id}/execute", post(crate::execute::execute))
        .route("/api/notebooks/run", post(crate::run::run))
        .route(
            "/api/host/grants/{*path}",
            get(crate::host::get_grant)
                .put(crate::host::put_grant)
                .delete(crate::host::delete_grant),
        )
        .route("/api/kernels/{id}/channels", get(crate::ws::channels))
        .route("/api/kernels/{id}/log", get(kernel_log))
        .route("/api/metrics/v1", get(crate::metrics::server))
        .route(
            "/api/metrics/v1/kernel_usage/get_usage/{id}",
            get(crate::metrics::kernel),
        )
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route("/api/terminals", get(list_terminals).post(create_terminal))
        .route(
            "/api/terminals/{name}",
            get(get_terminal).delete(delete_terminal),
        )
        .route("/terminals/websocket/{name}", get(crate::ws::terminal))
        .route("/lsp/status", get(lsp_status))
        .route("/lsp/ws/{name}", get(crate::ws::language_server))
        .route(
            "/api/contents",
            get(get_contents_root).post(post_contents_root),
        )
        // jupyter_server's path pattern takes the root with or without the
        // slash; `{*path}` below matches only a non-empty path.
        .route(
            "/api/contents/",
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
        .route(
            "/api/collaboration/session/{*path}",
            axum::routing::put(crate::collab::session),
        )
        .route("/api/collaboration/rooms", get(crate::collab::rooms))
        .route("/api/collaboration/room/{room}", get(crate::collab::room))
        .route("/api/fileid/index", post(crate::collab::fileid_index))
        .route("/api/fileid/id", get(crate::collab::fileid_id))
        .route("/api/fileid/path", get(crate::collab::fileid_path))
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
/// anonymous name per browser; this server has no users to tell apart,
/// except that a request let in by tailnet identity gets that name, so
/// JupyterLab shows it to collaborators.
/// `?permissions={"resource": ["action", ...]}` is answered with the
/// actions allowed, which is all of them: the token grants everything.
async fn me(
    Query(q): Query<MeQuery>,
    user: Option<axum::Extension<crate::auth::TailnetUser>>,
) -> HttpResult<Json<Value>> {
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
    let identity = match user {
        Some(axum::Extension(crate::auth::TailnetUser(name))) => {
            let initial: String = name
                .trim_start_matches("tag:")
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_else(|| "T".into());
            json!({
                "username": name,
                "name": name,
                "display_name": name,
                "initials": initial,
                "avatar_url": null,
                "color": null,
            })
        }
        None => json!({
            "username": "anonymous",
            "name": "Anonymous",
            "display_name": "Anonymous",
            "initials": "A",
            "avatar_url": null,
            "color": null,
        }),
    };
    Ok(Json(json!({
        "identity": identity,
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

/// Run blocking file I/O off the async workers.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> HttpResult<T> + Send + 'static,
) -> HttpResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| HttpError(500, e.to_string()))?
}

async fn get_config(
    State(s): State<AppState>,
    Path(section): Path<String>,
) -> HttpResult<Json<Value>> {
    let path = config_path(&s, &section)?;
    Ok(Json(blocking(move || Ok(read_config(&path))).await?))
}

/// Recursive merge; `null` deletes a key. PATCHes are serialized so two
/// at once cannot lose an update, and the file is replaced atomically so
/// a crash cannot truncate it.
async fn patch_config(
    State(s): State<AppState>,
    Path(section): Path<String>,
    LenientJson(update): LenientJson<Value>,
) -> HttpResult<Json<Value>> {
    static PATCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let path = config_path(&s, &section)?;
    let _serial = PATCH.lock().await;
    let current = blocking(move || {
        let mut current = read_config(&path);
        merge(&mut current, update);
        crate::lab::write_atomic(&path, &serde_json::to_vec_pretty(&current).unwrap())?;
        Ok(current)
    })
    .await?;
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

/// Export formats: nbconvert's `html`, `markdown` and `script`, without
/// Python templates (#235, #677).
async fn nbconvert() -> Json<Value> {
    let formats: serde_json::Map<String, Value> = ganymede_nbformat::FORMATS
        .iter()
        .map(|(name, mime)| ((*name).to_owned(), json!({"output_mimetype": mime})))
        .collect();
    Json(Value::Object(formats))
}

/// What a published page must not show, with what goes in its place: the
/// server root's absolute path, the home directory, and the token. A
/// notebook's outputs can hold them (tracebacks, a printed path).
pub(crate) fn server_redactions(s: &AppState) -> Vec<(String, String)> {
    let mut redact = publish_redactions(s.collab.root(), s.token.as_deref());
    // `mcp_token` too (#766), by the same rule as the server's token.
    let mcp_token = s.mcp.as_ref().and_then(|m| m.token());
    if let Some(token) = mcp_token.filter(|t| t.len() >= 8) {
        redact.push((token.to_owned(), "[token]".to_owned()));
    }
    redact
}

/// The server's redactions for a given root (`.`) and token (`[token]`),
/// with the home directory (`~`): `ganymede nb publish` passes the
/// notebook's directory. Each directory goes in both as given and
/// resolved, since outputs show whichever the kernel saw (on macOS a
/// directory under `/var` or `/tmp` is `/private/...` resolved). A root
/// such as `/` is left out by the exporter.
pub fn publish_redactions(
    root: Option<&std::path::Path>,
    token: Option<&str>,
) -> Vec<(String, String)> {
    let mut redact = Vec::new();
    let mut both = |dir: &std::path::Path, with: &str| {
        let mut spellings = vec![dir.to_path_buf()];
        spellings.extend(dir.canonicalize().ok().filter(|c| c != dir));
        for d in spellings {
            redact.push((d.to_string_lossy().into_owned(), with.to_owned()));
        }
    };
    if let Some(root) = root {
        both(root, ".");
    }
    if let Some(home) = dirs::home_dir() {
        both(&home, "~");
    }
    if let Some(token) = token.filter(|t| t.len() >= 8) {
        redact.push((token.to_owned(), "[token]".to_owned()));
    }
    redact
}

#[derive(Deserialize)]
struct NbconvertQuery {
    download: Option<String>,
    /// HTML only, as `nb publish --code`: `folded`, `shown` or `hidden`.
    code: Option<String>,
    /// HTML only: a line under the title, as `nb publish --byline`.
    byline: Option<String>,
    /// HTML only: `true` puts the redacted notebook copy and its Download
    /// notebook button in the page, as `nb publish` does by default.
    copy: Option<String>,
}

/// A page option from the query: absent when not given, empty, or the
/// export isn't HTML (other formats ignore the options).
fn page_option(html: bool, value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| html && !v.is_empty())
}

/// A query flag: `true` or `false`, any case.
fn query_flag(name: &str, value: Option<&str>) -> HttpResult<Option<bool>> {
    match value {
        None => Ok(None),
        Some(v) if v.eq_ignore_ascii_case("true") => Ok(Some(true)),
        Some(v) if v.eq_ignore_ascii_case("false") => Ok(Some(false)),
        Some(v) => Err(HttpError(
            400,
            format!("{name} must be true or false, not {v:?}"),
        )),
    }
}

/// `GET /nbconvert/{format}/{path}`: the notebook exported, as
/// `jupyter_server`'s `NbconvertFileHandler` returns it for JupyterLab's
/// Save and Export Notebook As menu. A path that isn't a notebook
/// redirects to `/files`. An export with files (Markdown with images) is
/// a zip of the text and the files, always as a download; otherwise the
/// text, as a download with `?download=true`. The HTML page takes `nb
/// publish`'s options as `code`, `byline` and `copy`; without them it
/// is the default page, code folded and no notebook copy.
async fn nbconvert_file(
    State(s): State<AppState>,
    Path((format, path)): Path<(String, String)>,
    Query(q): Query<NbconvertQuery>,
) -> HttpResult<Response> {
    if !ganymede_nbformat::FORMATS.iter().any(|(f, _)| *f == format) {
        return Err(HttpError(404, format!("No exporter for format: {format}")));
    }
    // The page's options, one rule for all three: empty is absent, case
    // doesn't matter, and other formats ignore them.
    let html = format == "html";
    let code: ganymede_nbformat::CodeMode = match page_option(html, q.code.as_deref()) {
        None => Default::default(),
        Some(c) => c.parse().map_err(|e: String| HttpError(400, e))?,
    };
    let copy = query_flag("copy", page_option(html, q.copy.as_deref()))?.unwrap_or(false);
    // The byline as given, as `nb publish` and MCP take it; trimmed only
    // to tell an empty one.
    let byline = q.byline.clone().filter(|b| html && !b.trim().is_empty());
    let path = path.trim_matches('/').to_owned();
    let model = s
        .contents
        .get(
            &path,
            GetOptions {
                content: true,
                ..GetOptions::default()
            },
        )
        .await?;
    if model.kind != ContentType::Notebook {
        let to = format!("{}files/{}", s.base_url, url_escape(&path));
        return Ok(axum::response::Redirect::to(&to).into_response());
    }
    let nb: ganymede_nbformat::Notebook =
        serde_json::from_value(model.content.unwrap_or(Value::Null))
            .map_err(|e| HttpError(500, format!("nbconvert failed: {e}")))?;
    let title = model
        .name
        .strip_suffix(".ipynb")
        .unwrap_or(&model.name)
        .to_owned();
    // Decoding a notebook's images can take a while: off the runtime.
    let export = {
        let title = title.clone();
        let redact = server_redactions(&s);
        blocking(move || {
            Ok(match format.as_str() {
                "html" => ganymede_nbformat::to_html(
                    &nb,
                    &title,
                    &ganymede_nbformat::HtmlOptions {
                        code,
                        byline,
                        redact,
                        download: copy,
                    },
                ),
                "markdown" => ganymede_nbformat::to_markdown(&nb, &title),
                _ => ganymede_nbformat::to_script(&nb),
            })
        })
        .await?
    };
    for skipped in &export.skipped {
        tracing::warn!("nbconvert: {path}: leaving out {skipped}: bad base64");
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    // The script's type comes from the notebook; keep anything it names,
    // such as text/html, from running as this origin. The HTML page may
    // also download, for its Download notebook button; its output frames
    // keep their own sandbox, which doesn't allow it.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(if html { PAGE_CSP } else { FILES_CSP }),
    );
    let filename = format!("{title}{}", export.extension);
    if !export.resources.is_empty() {
        let zip = blocking(move || nbconvert_zip(&filename, export)).await?;
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/zip"),
        );
        attachment(&mut headers, &format!("{title}.zip"));
        return Ok((headers, zip).into_response());
    }
    if q.download.is_some_and(|d| d.eq_ignore_ascii_case("true")) {
        attachment(&mut headers, &filename);
    }
    let content_type =
        header::HeaderValue::from_str(&format!("{}; charset=utf-8", export.mimetype)).unwrap_or(
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        );
    headers.insert(header::CONTENT_TYPE, content_type);
    Ok((headers, export.body).into_response())
}

fn attachment(headers: &mut HeaderMap, filename: &str) {
    let disposition = format!("attachment; filename*=utf-8''{}", url_escape(filename));
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
}

/// An export with its files in a zip, as nbconvert's `respond_zip`.
fn nbconvert_zip(filename: &str, export: ganymede_nbformat::Export) -> HttpResult<Vec<u8>> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let fail = |e: &dyn std::fmt::Display| HttpError(500, format!("nbconvert failed: {e}"));
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default();
    w.start_file(filename, opts).map_err(|e| fail(&e))?;
    w.write_all(export.body.as_bytes()).map_err(|e| fail(&e))?;
    let mut written = std::collections::HashSet::new();
    for r in export.resources {
        let bytes = match r.data {
            ganymede_nbformat::ResourceData::Text(t) => t.into_bytes(),
            ganymede_nbformat::ResourceData::Bytes(b) => b,
        };
        if !written.insert(r.path.clone()) {
            continue;
        }
        w.start_file(r.path, opts).map_err(|e| fail(&e))?;
        w.write_all(&bytes).map_err(|e| fail(&e))?;
    }
    Ok(w.finish().map_err(|e| fail(&e))?.into_inner())
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
    // Reads every kernelspec directory.
    Ok(Json(blocking(move || Ok(s.kernels.kernelspecs()?)).await?))
}

async fn kernelspec(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> HttpResult<Json<ganymede_api::KernelspecModel>> {
    Ok(Json(
        blocking(move || Ok(s.kernels.kernelspec(&name)?)).await?,
    ))
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
    // A kernelspec with no directory of its own (one the app registered,
    // or the gateway's) has no files: never read from the working directory.
    if dir.as_os_str().is_empty() {
        return Err(HttpError(
            404,
            format!("kernelspec {name} has no resources"),
        ));
    }
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

#[derive(Deserialize, Default)]
struct FilesQuery {
    download: Option<String>,
}

/// `GET /files/{path}`: a file's raw bytes, as `jupyter_server`'s
/// `FilesHandler` serves them for Download, Open in New Browser Tab and
/// relative links in Markdown. Goes through the contents manager, so root
/// confinement and the hidden-file policy are the contents API's; a
/// directory is a 400, as `cm.get(type="file")` answers there.
///
/// A contents manager that can open the file streams it from disk, a
/// chunk at a time, so a large file costs no memory. One that can't is
/// read whole with `get`. Both answer a single `bytes` `Range` the same
/// way (see [`parse_range`]).
async fn get_file(
    State(s): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<FilesQuery>,
    req_headers: HeaderMap,
) -> HttpResult<Response> {
    // Tornado's `get_argument("download", False)`: any non-empty value.
    let download = q.download.is_some_and(|d| !d.is_empty());
    if let Some(open) = s.contents.open_file(&path).await? {
        let (len, name) = (open.len, open.name);
        let read_err = |e: std::io::Error| HttpError(500, format!("reading {path}: {e}"));
        let name_for_sniff = name.clone();
        let (file, content_type, csp) = tokio::task::spawn_blocking(move || {
            let mut file = open.file;
            let (content_type, csp) = classify(&mut file, &name_for_sniff)?;
            Ok::<_, std::io::Error>((file, content_type, csp))
        })
        .await
        .map_err(|e| HttpError(500, format!("reading {path}: {e}")))?
        .map_err(read_err)?;
        let mut headers = file_headers(&content_type, csp, &name, download);
        let outcome = requested_range(&req_headers, len);
        let (status, window) = range_headers(&mut headers, len, outcome);
        let body = match window {
            Some((start, count)) => stream_file(file, start, count),
            None => Body::empty(),
        };
        return Ok((status, headers, body).into_response());
    }

    let model = s
        .contents
        .get(
            &path,
            GetOptions {
                content: true,
                type_hint: Some(ContentType::File),
                ..GetOptions::default()
            },
        )
        .await?;
    let base64 = model.format == Some(Format::Base64);
    let bytes = match model.content {
        Some(Value::String(c)) if base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(c)
                .map_err(|e| HttpError(500, format!("bad base64 from contents: {e}")))?
        }
        Some(Value::String(c)) => c.into_bytes(),
        _ => Vec::new(),
    };
    let content_type = file_content_type(&model.name, base64);
    let csp = if published_page(&content_type, &bytes) {
        PAGE_CSP
    } else {
        FILES_CSP
    };
    let mut headers = file_headers(&content_type, csp, &model.name, download);
    let len = bytes.len() as u64;
    let outcome = requested_range(&req_headers, len);
    let (status, window) = range_headers(&mut headers, len, outcome);
    let body = match window {
        Some((start, count)) => {
            // `count` is at most `len`, so both fit `usize`.
            let start = start as usize;
            Body::from(Bytes::from(bytes).slice(start..start + count as usize))
        }
        None => Body::empty(),
    };
    Ok((status, headers, body).into_response())
}

/// The largest piece of a file `/files` reads at once, and the size of the
/// pieces the UTF-8 check reads.
const FILE_CHUNK: usize = 64 * 1024;

/// A `/files` response's `Content-Type`, the policy and the disposition.
fn file_headers(content_type: &str, csp: &'static str, name: &str, download: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(content_type)
            .unwrap_or(header::HeaderValue::from_static("application/octet-stream")),
    );
    // As `FilesHandler.content_security_policy`: run HTML and SVG in an
    // opaque origin, so a file in the notebook folder can't act as the UI.
    // A page Ganymede published may also download, for its Download
    // notebook button (#758).
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(csp),
    );
    if download {
        let disposition = format!("attachment; filename*=utf-8''{}", url_escape(name));
        if let Ok(v) = header::HeaderValue::from_str(&disposition) {
            headers.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    headers
}

/// The `Content-Type` and policy of an open file, reading what they need
/// from its start: the whole file, in chunks, when its name gives no type
/// (text or binary), and the first 512 bytes of an HTML file (is it a
/// published page).
fn classify<R: std::io::Read + std::io::Seek>(
    file: &mut R,
    name: &str,
) -> std::io::Result<(String, &'static str)> {
    use std::io::{Read, SeekFrom};
    let content_type = match name_content_type(name) {
        Some(t) => t,
        None if is_utf8(file)? => "text/plain; charset=UTF-8".into(),
        None => "application/octet-stream".into(),
    };
    let mut head = Vec::new();
    if content_type.starts_with("text/html") {
        file.seek(SeekFrom::Start(0))?;
        file.by_ref()
            .take(PAGE_HEAD as u64)
            .read_to_end(&mut head)?;
    }
    let csp = if published_page(&content_type, &head) {
        PAGE_CSP
    } else {
        FILES_CSP
    };
    Ok((content_type, csp))
}

/// Whether the whole file is UTF-8, read in [`FILE_CHUNK`]s so memory stays
/// flat. It reads all of the file before the first byte is sent, even for a
/// `HEAD` or a `Range` request, so it runs only for a name that gives no
/// type. A character cut by the end of a chunk (`error_len` is `None`) is
/// carried into the next one; cut by the end of the file, it is invalid.
fn is_utf8<R: std::io::Read + std::io::Seek>(file: &mut R) -> std::io::Result<bool> {
    use std::io::SeekFrom;
    file.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; FILE_CHUNK];
    let mut carried = 0;
    loop {
        let n = file.read(&mut buf[carried..])?;
        if n == 0 {
            return Ok(carried == 0);
        }
        let filled = carried + n;
        match std::str::from_utf8(&buf[..filled]) {
            Ok(_) => carried = 0,
            Err(e) if e.error_len().is_some() => return Ok(false),
            Err(e) => {
                let valid = e.valid_up_to();
                buf.copy_within(valid..filled, 0);
                carried = filled - valid;
            }
        }
    }
}

/// A body reading `count` bytes of `file` from `start`, [`FILE_CHUNK`] at a
/// time. A file that ends early (it was truncated while being sent) fails
/// the stream, which drops the connection rather than send a short body
/// as if it were whole.
fn stream_file(file: std::fs::File, start: u64, count: u64) -> Body {
    stream_from(tokio::fs::File::from_std(file), start, count)
}

/// [`stream_file`] over any seekable reader, so a test can count what it
/// reads.
fn stream_from<R>(file: R, start: u64, count: u64) -> Body
where
    R: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin + Send + 'static,
{
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    struct State<R> {
        file: R,
        seek: Option<u64>,
        remaining: u64,
    }
    let state = State {
        file,
        seek: Some(start),
        remaining: count,
    };
    let chunks = futures::stream::unfold(Some(state), |state| async move {
        let mut st = state?;
        if st.remaining == 0 {
            return None;
        }
        if let Some(pos) = st.seek.take() {
            if let Err(e) = st.file.seek(std::io::SeekFrom::Start(pos)).await {
                return Some((Err(e), None));
            }
        }
        let want = st.remaining.min(FILE_CHUNK as u64) as usize;
        let mut buf = vec![0u8; want];
        match st.file.read(&mut buf).await {
            Ok(0) => Some((Err(std::io::ErrorKind::UnexpectedEof.into()), None)),
            Ok(n) => {
                buf.truncate(n);
                st.remaining -= n as u64;
                Some((Ok(Bytes::from(buf)), Some(st)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    Body::from_stream(chunks)
}

/// What a request's `Range` asks of a file.
#[derive(Debug, PartialEq, Eq)]
enum RangeOutcome {
    /// No usable range: send the whole file with 200.
    Whole,
    /// Bytes `start..=end`, both inside the file: 206.
    Partial(u64, u64),
    /// Nothing of the file is in the range: 416.
    Unsatisfiable,
}

/// The range to answer for `headers` on a file of `len` bytes. An
/// `If-Range` makes the range conditional on a validator, which `/files`
/// doesn't send, so it is ignored (RFC 9110 section 13.1.5).
fn requested_range(headers: &HeaderMap, len: u64) -> RangeOutcome {
    if headers.contains_key(header::IF_RANGE) {
        return RangeOutcome::Whole;
    }
    headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map_or(RangeOutcome::Whole, |v| parse_range(v, len))
}

/// Parse a `Range` header value against a file of `len` bytes (RFC 9110
/// section 14). Only one range of the `bytes` unit is answered: `a-b`,
/// `a-` or the suffix `-n`. Anything else is ignored, which the RFC
/// allows, and the whole file is sent: another unit, more than one range,
/// a malformed spec, `a-b` with `a > b`, or a number that doesn't fit a
/// `u64`. A range that starts past the end, a suffix of zero bytes, and
/// any range of an empty file are unsatisfiable. An end past the last byte
/// is the last byte.
fn parse_range(value: &str, len: u64) -> RangeOutcome {
    use RangeOutcome::{Partial, Unsatisfiable, Whole};
    let Some((unit, spec)) = value.split_once('=') else {
        return Whole;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return Whole;
    }
    let number = |t: &str| -> Option<u64> {
        let t = t.trim();
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        t.parse().ok()
    };
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Whole;
    };
    let (start, end) = if first.trim().is_empty() {
        // Suffix: the last `n` bytes.
        let Some(n) = number(last) else {
            return Whole;
        };
        if n == 0 || len == 0 {
            return Unsatisfiable;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let Some(a) = number(first) else {
            return Whole;
        };
        let b = if last.trim().is_empty() {
            None
        } else {
            match number(last) {
                Some(b) if b >= a => Some(b),
                _ => return Whole,
            }
        };
        if a >= len {
            return Unsatisfiable;
        }
        (a, b.map_or(len - 1, |b| b.min(len - 1)))
    };
    Partial(start, end)
}

/// Set the status-dependent headers for `outcome` on a file of `len`
/// bytes: `Accept-Ranges` always, `Content-Length` and, for a range,
/// `Content-Range`. Returns the status and the window `(start, count)` to
/// send, `None` for a 416, which has no body.
fn range_headers(
    headers: &mut HeaderMap,
    len: u64,
    outcome: RangeOutcome,
) -> (StatusCode, Option<(u64, u64)>) {
    headers.insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    let (status, window, content_range) = match outcome {
        RangeOutcome::Whole => (StatusCode::OK, Some((0, len)), None),
        RangeOutcome::Partial(a, b) => (
            StatusCode::PARTIAL_CONTENT,
            Some((a, b - a + 1)),
            Some(format!("bytes {a}-{b}/{len}")),
        ),
        RangeOutcome::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            None,
            Some(format!("bytes */{len}")),
        ),
    };
    if let Some(v) = content_range.and_then(|v| header::HeaderValue::from_str(&v).ok()) {
        headers.insert(header::CONTENT_RANGE, v);
    }
    if let Some((_, count)) = window {
        headers.insert(header::CONTENT_LENGTH, header::HeaderValue::from(count));
    }
    (status, window)
}

/// The `Content-Security-Policy` on every `/files` response, as
/// jupyter_server's `FilesHandler` appends to its policy.
pub(crate) const FILES_CSP: &str = "sandbox allow-scripts";

/// The published page's policy from `/nbconvert/html`, and from `/files`
/// for a page `nb publish` wrote: [`FILES_CSP`], and downloads for its
/// Download notebook button.
pub(crate) const PAGE_CSP: &str = "sandbox allow-scripts allow-downloads";

/// How much of an HTML file's start holds a published page's generator tag.
const PAGE_HEAD: usize = 512;

/// An HTML file that begins as a page Ganymede published does: its
/// generator tag in the first lines of its head. Any page can copy the
/// tag, which gains it downloads and nothing else: it still runs in an
/// opaque origin.
fn published_page(content_type: &str, bytes: &[u8]) -> bool {
    content_type.starts_with("text/html")
        && String::from_utf8_lossy(&bytes[..bytes.len().min(PAGE_HEAD)])
            .contains(ganymede_nbformat::HTML_GENERATOR_MARK)
}

/// `FilesHandler`'s `Content-Type`: [`name_content_type`], or with no
/// guess from the name, by the format the contents manager read it in.
fn file_content_type(name: &str, base64: bool) -> String {
    name_content_type(name).unwrap_or_else(|| {
        if base64 {
            "application/octet-stream".into()
        } else {
            "text/plain; charset=UTF-8".into()
        }
    })
}

/// The part of `FilesHandler`'s `Content-Type` the name decides: `.ipynb`
/// is `application/x-ipynb+json`; otherwise the type guessed from the
/// name, with a compression suffix (Python's `mimetypes` encodings) giving
/// `application/gzip` for `.gz` and `application/octet-stream` for the
/// rest. `None` when the name gives no type.
fn name_content_type(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".ipynb") {
        return Some("application/x-ipynb+json".into());
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "gz" => return Some("application/gzip".into()),
        "z" | "bz2" | "xz" | "br" => return Some("application/octet-stream".into()),
        _ => {}
    }
    match mime_guess::from_path(&lower).first_raw() {
        Some("text/plain") => Some("text/plain; charset=UTF-8".into()),
        Some(m) => Some(m.into()),
        None => None,
    }
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

#[derive(Deserialize)]
struct KernelLogQuery {
    after: Option<u64>,
    lines: Option<usize>,
}

/// Lines a log request returns when it names no `lines`.
const KERNEL_LOG_DEFAULT_LINES: usize = 200;

/// A Ganymede extension: the kernel's recent stdout and stderr and the
/// server's lines about its process. Jupyter Server has no such route.
async fn kernel_log(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<KernelLogQuery>,
) -> HttpResult<Json<ganymede_api::KernelLogModel>> {
    let lines = q
        .lines
        .unwrap_or(KERNEL_LOG_DEFAULT_LINES)
        .min(ganymede_api::kernel_log::LOG_LINES);
    Ok(Json(s.kernels.log(&id, q.after, lines)?))
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
) -> HttpResult<Response> {
    get_contents(state, Path(String::new()), q).await
}

async fn get_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<ContentsQuery>,
) -> HttpResult<Response> {
    match target(&path) {
        ContentsTarget::Checkpoints(p) => {
            Ok(Json(s.contents.list_checkpoints(&p).await?).into_response())
        }
        ContentsTarget::Checkpoint(..) => Err(HttpError(404, "not found".into())),
        ContentsTarget::Trust(_) => Err(method_not_allowed()),
        ContentsTarget::Path(p) => {
            let content = q.content.as_deref().map(|c| c != "0").unwrap_or(true);
            let hash = q.hash.as_deref().is_some_and(|h| h != "0");
            // With content, hash it too, for the ETag a later If-Match names.
            let mut model = s
                .contents
                .get(
                    &p,
                    GetOptions {
                        content,
                        type_hint: q.type_,
                        format: q.format,
                        hash: hash || content,
                    },
                )
                .await?;
            // A notebook whose room another server owns can only be read
            // here (#272): JupyterLab shows it as read-only.
            if model.writable
                && model.kind == ContentType::Notebook
                && s.collab.owned_elsewhere(&model.path)
            {
                model.writable = false;
            }
            Ok(with_etag(model, hash))
        }
    }
}

/// The model as JSON with its sha256 as a strong `ETag`, which the client
/// can send back as `If-Match` on a save (see [`if_match`]). jupyter_server
/// has no such precondition, so `hash` stays in the body only when the
/// client asked for it (`?hash=1`).
fn with_etag(mut model: ganymede_contents::ContentsModel, keep_hash: bool) -> Response {
    let etag = model
        .hash
        .as_deref()
        .and_then(|h| header::HeaderValue::from_str(&format!("\"{h}\"")).ok());
    if !keep_hash {
        model.hash = None;
        model.hash_algorithm = None;
    }
    let mut resp = Json(model).into_response();
    if let Some(etag) = etag {
        resp.headers_mut().insert(header::ETAG, etag);
    }
    resp
}

/// Parse `If-Match` (RFC 9110 13.1.1). Weak tags never match a strong
/// comparison, so they are dropped; a header of only weak tags fails.
fn if_match(headers: &HeaderMap) -> Option<IfMatch> {
    let values: Vec<&str> = headers
        .get_all(header::IF_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if values.is_empty() {
        return None;
    }
    let mut tags = Vec::new();
    for tag in values.iter().flat_map(|v| v.split(',')).map(str::trim) {
        if tag == "*" {
            return Some(IfMatch::Any);
        }
        if let Some(t) = tag.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            tags.push(t.to_string());
        }
    }
    Some(IfMatch::Hashes(tags))
}

async fn put_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
    LenientJson(req): LenientJson<Option<SaveRequest>>,
) -> HttpResult<Response> {
    let p = match target(&path) {
        ContentsTarget::Path(p) => p,
        ContentsTarget::Trust(_) => return Err(method_not_allowed()),
        _ => return Err(HttpError(400, "cannot PUT a checkpoint".into())),
    };
    let mut req =
        req.ok_or_else(|| HttpError(400, "PUT needs a body with type, format, content".into()))?;
    req.if_match = if_match(&headers);
    let existed = s.contents.get(&p, GetOptions::default()).await.is_ok();
    let model = s.contents.save(&p, req).await?;
    let location = (!existed).then(|| contents_location(&s, &model.path));
    let mut resp = with_etag(model, false);
    if let Some(location) = location {
        *resp.status_mut() = StatusCode::CREATED;
        if let Ok(v) = header::HeaderValue::from_str(&location) {
            resp.headers_mut().insert(header::LOCATION, v);
        }
    }
    Ok(resp)
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
    let new_path = ganymede_contents::normalize_api_path(&new_path);
    Ok(Json(s.collab.rename(&s.contents, &p, &new_path).await?))
}

async fn delete_contents(
    State(s): State<AppState>,
    Path(path): Path<String>,
) -> HttpResult<StatusCode> {
    match target(&path) {
        ContentsTarget::Path(p) => s.collab.delete(&s.contents, &p).await?,
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

// ----- terminals -------------------------------------------------------------

impl From<ganymede_terminals::Error> for HttpError {
    fn from(e: ganymede_terminals::Error) -> Self {
        HttpError(e.status(), e.to_string())
    }
}

/// The terminal manager, or 404 as `jupyter_server` answers with
/// `terminals_enabled` off: it registers no terminal routes then.
pub(crate) fn terminals(s: &AppState) -> HttpResult<&ganymede_terminals::TerminalManager> {
    s.terminals
        .as_deref()
        .ok_or_else(|| HttpError(404, "Not Found".into()))
}

fn terminal_model(t: ganymede_terminals::TerminalModel) -> Value {
    json!({"name": t.name, "last_activity": t.last_activity})
}

async fn list_terminals(State(s): State<AppState>) -> HttpResult<Json<Value>> {
    let list = terminals(&s)?.list();
    Ok(Json(Value::Array(
        list.into_iter().map(terminal_model).collect(),
    )))
}

#[derive(Deserialize, Default)]
struct TerminalCreate {
    name: Option<String>,
    cwd: Option<String>,
}

/// `POST /api/terminals`, body `{"name"?, "cwd"?}`: 200 with the model,
/// as `jupyter_server` answers.
async fn create_terminal(
    State(s): State<AppState>,
    LenientJson(body): LenientJson<TerminalCreate>,
) -> HttpResult<Json<Value>> {
    let Some(mgr) = s.terminals.clone() else {
        return Err(HttpError(404, "Not Found".into()));
    };
    let model = blocking(move || Ok(mgr.create(body.name, body.cwd.as_deref())?)).await?;
    Ok(Json(terminal_model(model)))
}

async fn get_terminal(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> HttpResult<Json<Value>> {
    Ok(Json(terminal_model(terminals(&s)?.get(&name)?)))
}

async fn delete_terminal(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> HttpResult<StatusCode> {
    terminals(&s)?.delete(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- language servers -------------------------------------------------------

impl From<ganymede_lsp::Error> for HttpError {
    fn from(e: ganymede_lsp::Error) -> Self {
        HttpError(e.status(), e.to_string())
    }
}

/// The language server manager, or 404 as a server without `jupyter-lsp`
/// answers: none is configured.
pub(crate) fn lsp(s: &AppState) -> HttpResult<&ganymede_lsp::LspManager> {
    s.lsp
        .as_deref()
        .ok_or_else(|| HttpError(404, "Not Found".into()))
}

/// `GET /lsp/status`: the configured servers and their sessions, as
/// `jupyter-lsp` reports them.
async fn lsp_status(State(s): State<AppState>) -> HttpResult<Json<Value>> {
    Ok(Json(lsp(&s)?.status()))
}

#[cfg(test)]
mod tests {
    use super::RangeOutcome::{Partial, Unsatisfiable, Whole};
    use super::*;

    #[test]
    fn range_forms() {
        assert_eq!(parse_range("bytes=0-3", 10), Partial(0, 3));
        assert_eq!(parse_range("bytes=5-", 10), Partial(5, 9));
        assert_eq!(parse_range("bytes=-4", 10), Partial(6, 9));
        assert_eq!(parse_range("bytes=9-9", 10), Partial(9, 9));
        assert_eq!(parse_range(" bytes = 1 - 2 ", 10), Partial(1, 2));
        assert_eq!(parse_range("BYTES=1-2", 10), Partial(1, 2));
    }

    #[test]
    fn range_clamps_to_the_file() {
        assert_eq!(parse_range("bytes=7-100", 10), Partial(7, 9));
        assert_eq!(parse_range("bytes=0-10", 10), Partial(0, 9));
        // A suffix longer than the file is the whole file, as a 206.
        assert_eq!(parse_range("bytes=-100", 10), Partial(0, 9));
    }

    #[test]
    fn range_unsatisfiable() {
        assert_eq!(parse_range("bytes=10-", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=100-200", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 10), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), Unsatisfiable);
    }

    #[test]
    fn range_ignored_when_malformed_or_unsupported() {
        for v in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "bytes=abc",
            "bytes=1",
            "bytes=a-b",
            "bytes=+1-2",
            "bytes=1--2",
            "bytes=5-2",
            "items=0-1",
            "bytes=0-1,4-5",
            "bytes=0-1, -2",
            // Doesn't fit a u64: malformed, so the whole file.
            "bytes=99999999999999999999-",
            "bytes=0-99999999999999999999",
            "bytes=-99999999999999999999",
        ] {
            assert_eq!(parse_range(v, 10), Whole, "{v:?}");
        }
        // An a > b spec is malformed even when a is past the end.
        assert_eq!(parse_range("bytes=50-20", 10), Whole);
        assert_eq!(
            parse_range("bytes=18446744073709551615-", 10),
            Unsatisfiable
        );
    }

    #[test]
    fn if_range_ignores_the_range() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=0-1".parse().unwrap());
        assert_eq!(requested_range(&h, 10), Partial(0, 1));
        h.insert(header::IF_RANGE, "\"x\"".parse().unwrap());
        assert_eq!(requested_range(&h, 10), Whole);
    }

    /// A file that ends before `count` bytes fails the stream, so the
    /// connection drops instead of a short body passing for a whole one.
    #[tokio::test]
    async fn a_file_cut_short_fails_the_stream() {
        use futures::StreamExt;
        let mut f = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut f, b"0123456789").unwrap();
        let mut items = stream_file(f, 4, 100).into_data_stream();
        let mut got = Vec::new();
        let mut last = None;
        while let Some(item) = items.next().await {
            if let Ok(b) = &item {
                got.extend_from_slice(b);
            }
            last = Some(item);
        }
        assert_eq!(got, b"456789");
        assert!(
            last.unwrap().is_err(),
            "a short file ended the body quietly"
        );
    }

    /// Reads that record the largest buffer they are given and the bytes
    /// they return.
    struct Spy {
        inner: std::io::Cursor<Vec<u8>>,
        largest: usize,
        read: usize,
    }

    impl Spy {
        fn new(data: Vec<u8>) -> Self {
            Spy {
                inner: std::io::Cursor::new(data),
                largest: 0,
                read: 0,
            }
        }
    }

    impl std::io::Read for Spy {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.largest = self.largest.max(buf.len());
            let n = self.inner.read(buf)?;
            self.read += n;
            Ok(n)
        }
    }

    impl std::io::Seek for Spy {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn the_utf8_check_reads_in_bounded_chunks() {
        let mut spy = Spy::new(vec![b'a'; 1 << 20]);
        assert!(is_utf8(&mut spy).unwrap());
        assert!(
            spy.largest > 0 && spy.largest <= FILE_CHUNK,
            "{}",
            spy.largest
        );
    }
    /// An HTML file is read only as far as its first [`PAGE_HEAD`] bytes to
    /// tell a published page, however large it is.
    #[test]
    fn an_html_file_is_classified_from_its_head_only() {
        let mut page = b"<!doctype html><html><body>".to_vec();
        page.resize(1 << 20, b'x');
        let mut spy = Spy::new(page);
        let (content_type, _) = classify(&mut spy, "big.html").unwrap();
        assert!(content_type.starts_with("text/html"), "{content_type}");
        assert!(spy.read <= PAGE_HEAD, "read {} bytes", spy.read);
    }

    /// An in-memory reader that counts the bytes it returns.
    struct Counting {
        inner: std::io::Cursor<Vec<u8>>,
        read: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl tokio::io::AsyncRead for Counting {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let n = std::io::Read::read(&mut self.inner, buf.initialize_unfilled())?;
            buf.advance(n);
            self.read.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl tokio::io::AsyncSeek for Counting {
        fn start_seek(
            mut self: std::pin::Pin<&mut Self>,
            pos: std::io::SeekFrom,
        ) -> std::io::Result<()> {
            std::io::Seek::seek(&mut self.inner, pos).map(|_| ())
        }

        fn poll_complete(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<u64>> {
            std::task::Poll::Ready(Ok(self.inner.position()))
        }
    }

    /// A range at the end of a large file reads only that range, not the
    /// bytes before it (reading a Parquet footer, say).
    #[tokio::test]
    async fn a_range_at_the_end_reads_only_that_range() {
        use futures::StreamExt;
        let read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut data = vec![0u8; 1 << 20];
        let len = data.len() as u64;
        data[(len - 8) as usize..].copy_from_slice(b"PAR1tail");
        let reader = Counting {
            inner: std::io::Cursor::new(data),
            read: read.clone(),
        };
        let mut items = stream_from(reader, len - 8, 8).into_data_stream();
        let mut got = Vec::new();
        while let Some(item) = items.next().await {
            got.extend_from_slice(&item.unwrap());
        }
        assert_eq!(got, b"PAR1tail");
        assert_eq!(read.load(std::sync::atomic::Ordering::SeqCst), 8);
    }
}
