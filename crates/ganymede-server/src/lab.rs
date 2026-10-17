//! Serving JupyterLab's own frontend: the page with its page config, the
//! static bundle, themes, prebuilt (federated) extensions, and the
//! `/lab/api/*` endpoints `jupyterlab_server` provides. The endpoint set
//! and response shapes follow `jupyterlab_server` 2.x as JupyterLab 4
//! calls it; Jupyverse documents the same list.
//!
//! JupyterLab's application directory (`share/jupyter/lab`) holds
//! `static/`, `schemas/`, and `themes/`. Pointing `--static-dir` at its
//! `static/` directory turns this on; [`Lab::detect`] recognises the
//! layout.

use crate::routes::HttpError;
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};
use tower_http::services::ServeDir;

/// Message types JupyterLab's kernel client may drop from its pending
/// message tracking. Copied from `jupyterlab_server`'s page config.
const UNTRACKED_MESSAGE_TYPES: &[&str] = &[
    "comm_info_request",
    "comm_info_reply",
    "kernel_info_request",
    "kernel_info_reply",
    "shutdown_request",
    "shutdown_reply",
    "interrupt_request",
    "interrupt_reply",
    "debug_request",
    "debug_reply",
    "stream",
    "display_data",
    "update_display_data",
    "execute_input",
    "execute_result",
    "error",
    "status",
    "clear_output",
    "debug_event",
    "input_request",
    "input_reply",
];

/// A prebuilt extension found in a `labextensions` directory.
#[derive(Debug, Clone)]
pub struct FederatedExtension {
    pub name: String,
    pub dir: PathBuf,
    /// The `jupyterlab._build` object from its `package.json`.
    pub build: Map<String, Value>,
    pub version: String,
}

/// JupyterLab's application directory and the state it needs.
#[derive(Debug, Clone)]
pub struct Lab {
    pub static_dir: PathBuf,
    pub schemas_dir: PathBuf,
    pub themes_dir: PathBuf,
    pub app_name: String,
    pub app_version: String,
    /// The packaged `index.html`, a Jinja template.
    template: String,
    pub extensions: Vec<FederatedExtension>,
    /// `lab/user-settings` under the Jupyter config dir.
    pub user_settings_dir: PathBuf,
    /// `lab/workspaces` under the Jupyter config dir.
    pub workspaces_dir: PathBuf,
    /// Absolute contents root, reported as `serverRoot`.
    pub server_root: PathBuf,
}

impl Lab {
    /// `Some` when `static_dir` is JupyterLab's static bundle: an
    /// `index.html` carrying `jupyter-config-data` and a `package.json`
    /// with a `jupyterlab` section.
    pub fn detect(
        static_dir: &FsPath,
        labextension_dirs: &[PathBuf],
        config_dir: &FsPath,
        server_root: &FsPath,
    ) -> Option<Lab> {
        let template = std::fs::read_to_string(static_dir.join("index.html")).ok()?;
        if !template.contains("jupyter-config-data") {
            return None;
        }
        let pkg = read_json(&static_dir.join("package.json"))?;
        let lab = pkg.get("jupyterlab")?;
        let app_dir = static_dir.parent()?;
        Some(Lab {
            static_dir: static_dir.to_path_buf(),
            schemas_dir: app_dir.join("schemas"),
            themes_dir: app_dir.join("themes"),
            app_name: lab
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("JupyterLab")
                .to_string(),
            app_version: lab
                .get("version")
                .or_else(|| pkg.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            template,
            extensions: find_extensions(labextension_dirs),
            user_settings_dir: config_dir.join("lab").join("user-settings"),
            workspaces_dir: config_dir.join("lab").join("workspaces"),
            server_root: server_root.to_path_buf(),
        })
    }

    fn page_config(
        &self,
        base_url: &str,
        token: Option<&str>,
        tree_path: &str,
        workspace: &str,
    ) -> Value {
        let full = |p: &str| format!("{base_url}{p}");
        let federated: Vec<Value> = self
            .extensions
            .iter()
            .map(|e| {
                let get = |k: &str| e.build.get(k).cloned().unwrap_or(Value::Null);
                let mut m = Map::new();
                m.insert("name".into(), e.name.clone().into());
                m.insert("load".into(), get("load"));
                m.insert("entrypoints".into(), get("entrypoints"));
                for k in ["extension", "mimeExtension", "style"] {
                    if let Some(v) = e.build.get(k) {
                        m.insert(k.into(), v.clone());
                    }
                }
                Value::Object(m)
            })
            .collect();
        let root = self.server_root.to_string_lossy();
        json!({
            "allow_hidden_files": false,
            "appName": self.app_name,
            "appNamespace": "lab",
            "appUrl": "/lab",
            "appVersion": self.app_version,
            "baseUrl": base_url,
            "buildAvailable": false,
            "buildCheck": false,
            "cacheFiles": false,
            "copyAbsolutePath": false,
            "delete_to_trash": false,
            "devMode": false,
            "disabledExtensions": [],
            "exposeAppInBrowser": false,
            "extensionManager": {"name": "read-only", "can_install": false, "install_path": null},
            "federated_extensions": federated,
            "fullAppUrl": full("lab"),
            "fullLabextensionsUrl": full("lab/extensions"),
            "fullLicensesUrl": full("lab/api/licenses"),
            "fullListingsUrl": full("lab/api/listings"),
            "fullMathjaxUrl": "https://cdnjs.cloudflare.com/ajax/libs/mathjax/2.7.7/MathJax.js",
            "fullSettingsUrl": full("lab/api/settings"),
            "fullStaticUrl": full("static/lab"),
            "fullThemesUrl": full("lab/api/themes"),
            "fullTranslationsApiUrl": full("lab/api/translations"),
            "fullTreeUrl": full("lab/tree"),
            "fullWorkspacesApiUrl": full("lab/api/workspaces"),
            "ignorePlugins": [],
            "labextensionsUrl": "/lab/extensions",
            "licensesUrl": "/lab/api/licenses",
            "listingsUrl": "/lab/api/listings",
            "mathjaxConfig": "TeX-AMS_HTML-full,Safe",
            "mode": "multiple-document",
            "news": {"disabled": true},
            "notebookStartsKernel": true,
            "preferredPath": "/",
            "quitButton": false,
            "rootUri": format!("file://{root}"),
            "serverRoot": root,
            "settingsUrl": "/lab/api/settings",
            "store_id": 0,
            "terminalsAvailable": false,
            "themesUrl": "/lab/api/themes",
            "token": token.unwrap_or(""),
            "translationsApiUrl": "/lab/api/translations",
            "treePath": tree_path,
            "treeUrl": "/lab/tree",
            "untracked_message_types": UNTRACKED_MESSAGE_TYPES,
            "workspace": workspace,
            "workspacesApiUrl": "/lab/api/workspaces",
            "wsUrl": "",
        })
    }

    fn render_page(
        &self,
        base_url: &str,
        token: Option<&str>,
        tree_path: &str,
        workspace: &str,
    ) -> String {
        let config = self.page_config(base_url, token, tree_path, workspace);
        render_template(&self.template, &config, base_url)
    }

    // ----- settings ----------------------------------------------------

    /// Every settings schema: the application's, then each prebuilt
    /// extension's. `(id, schema file, version)`.
    fn schema_files(&self) -> Vec<(String, PathBuf, String)> {
        let mut out = schemas_under(&self.schemas_dir, &self.app_version);
        for e in &self.extensions {
            out.extend(schemas_under(&e.dir.join("schemas"), &e.version));
        }
        out
    }

    fn user_settings_path(&self, id: &str) -> Option<PathBuf> {
        let (pkg, plugin) = id.split_once(':')?;
        if !safe_segments(pkg) || !safe_segments(plugin) || plugin.contains('/') {
            return None;
        }
        Some(
            self.user_settings_dir
                .join(pkg)
                .join(format!("{plugin}.jupyterlab-settings")),
        )
    }

    fn plugin_settings(&self, id: &str, schema_file: &FsPath, version: &str) -> Value {
        let schema = read_json(schema_file).unwrap_or_else(|| json!({}));
        let user = self.user_settings_path(id);
        let raw = user
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| "{}".into());
        let settings: Value = json5::from_str(&raw).unwrap_or_else(|_| json!({}));
        let meta = user.as_ref().and_then(|p| std::fs::metadata(p).ok());
        let time = |t: Option<std::time::SystemTime>| -> Value {
            t.map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339().into())
                .unwrap_or(Value::Null)
        };
        json!({
            "id": id,
            "raw": raw,
            "schema": schema,
            "settings": settings,
            "version": version,
            "last_modified": time(meta.as_ref().and_then(|m| m.modified().ok())),
            "created": time(meta.as_ref().and_then(|m| m.created().ok())),
        })
    }

    // ----- workspaces --------------------------------------------------

    fn workspace_path(&self, id: &str) -> PathBuf {
        let slug: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.workspaces_dir
            .join(format!("{slug}.jupyterlab-workspace"))
    }
}

/// Routes for the page, assets, and `/lab/api/*`. Assets are public, as
/// in `jupyter_server`; the page sends a browser without a login to
/// `/login`, and the API answers 403.
pub fn router(state: AppState, lab: &Lab) -> Router {
    let pages = Router::new()
        .route("/lab", get(page_root))
        .route("/lab/tree", get(page_root))
        .route("/lab/tree/{*path}", get(page_tree))
        .route("/lab/workspaces/{*name}", get(page_workspace))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::page,
        ));
    let api = Router::new()
        .route("/lab/api/settings", get(list_settings))
        .route(
            "/lab/api/settings/{*id}",
            get(get_settings).put(put_settings),
        )
        .route("/lab/api/workspaces", get(list_workspaces))
        .route(
            "/lab/api/workspaces/{*id}",
            get(get_workspace)
                .put(put_workspace)
                .delete(delete_workspace),
        )
        .route("/lab/api/translations", get(list_translations))
        .route("/lab/api/translations/{locale}", get(get_translation))
        .route("/lab/api/listings/{*path}", get(listings))
        .route("/lab/api/plugins", get(plugins))
        .route("/lab/api/licenses", get(licenses))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::api,
        ));
    let mut public =
        Router::new()
            .route(
                "/",
                get(|State(s): State<AppState>| async move {
                    Redirect::to(&format!("{}lab", s.base_url))
                }),
            )
            .nest_service("/static/lab", ServeDir::new(&lab.static_dir))
            .route("/lab/api/themes/{*path}", get(theme_file));
    for e in &lab.extensions {
        public = public.nest_service(
            &format!("/lab/extensions/{}", e.name),
            ServeDir::new(&e.dir),
        );
    }
    pages.merge(api).merge(public).with_state(state)
}

fn lab_of(s: &AppState) -> HttpResult<&Lab> {
    s.lab
        .as_deref()
        .ok_or_else(|| HttpError(404, "JupyterLab is not configured".into()))
}

type HttpResult<T> = std::result::Result<T, HttpError>;

// ----- page -----------------------------------------------------------------

async fn page_root(State(s): State<AppState>, headers: HeaderMap) -> HttpResult<Response> {
    page(&s, &headers, "", "default")
}

async fn page_tree(
    State(s): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> HttpResult<Response> {
    page(&s, &headers, &path, "default")
}

async fn page_workspace(
    State(s): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> HttpResult<Response> {
    // `/lab/workspaces/{name}/tree/{path}` opens a path in a workspace.
    let (name, tree) = name.split_once("/tree/").unwrap_or((&name, ""));
    page(&s, &headers, tree, name)
}

/// The page, with an `_xsrf` cookie when the browser has none, as
/// `jupyter_server` sets it on every page it renders.
fn page(
    s: &AppState,
    headers: &HeaderMap,
    tree_path: &str,
    workspace: &str,
) -> HttpResult<Response> {
    let lab = lab_of(s)?;
    let html = Html(lab.render_page(&s.base_url, s.token.as_deref(), tree_path, workspace));
    Ok(match crate::auth::xsrf_cookie(s, headers) {
        Some(cookie) => ([(header::SET_COOKIE, cookie)], html).into_response(),
        None => html.into_response(),
    })
}

// ----- themes ---------------------------------------------------------------

/// `/lab/api/themes/{package}/{file}`: the application's themes, then
/// each prebuilt extension's `themes/` directory.
async fn theme_file(State(s): State<AppState>, Path(path): Path<String>) -> Response {
    let Ok(lab) = lab_of(&s) else {
        return HttpError(404, "not found".into()).into_response();
    };
    if !safe_segments(&path) {
        return HttpError(404, "not found".into()).into_response();
    }
    let mut candidates = vec![lab.themes_dir.join(&path)];
    candidates.extend(
        lab.extensions
            .iter()
            .map(|e| e.dir.join("themes").join(&path)),
    );
    for c in candidates {
        if let Ok(bytes) = tokio::fs::read(&c).await {
            let mime = mime_for(&c);
            return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
        }
    }
    HttpError(404, format!("no theme file {path}")).into_response()
}

fn mime_for(p: &FsPath) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

// ----- settings -------------------------------------------------------------

#[derive(Deserialize, Default)]
struct SettingsQuery {
    ids_only: Option<String>,
}

async fn list_settings(
    State(s): State<AppState>,
    Query(q): Query<SettingsQuery>,
) -> HttpResult<Json<Value>> {
    let lab = lab_of(&s)?;
    let files = lab.schema_files();
    let ids_only = matches!(q.ids_only.as_deref(), Some("true" | "1"));
    let settings: Vec<Value> = files
        .iter()
        .map(|(id, file, version)| {
            if ids_only {
                json!({"id": id})
            } else {
                lab.plugin_settings(id, file, version)
            }
        })
        .collect();
    Ok(Json(json!({ "settings": settings })))
}

async fn get_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<Json<Value>> {
    let lab = lab_of(&s)?;
    let (_, file, version) = lab
        .schema_files()
        .into_iter()
        .find(|(i, ..)| *i == id)
        .ok_or_else(|| HttpError(404, format!("Schema not found: {id}")))?;
    Ok(Json(lab.plugin_settings(&id, &file, &version)))
}

#[derive(Deserialize, Default)]
struct RawSettings {
    raw: Option<String>,
}

async fn put_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
    crate::routes::LenientJson(body): crate::routes::LenientJson<RawSettings>,
) -> HttpResult<StatusCode> {
    let lab = lab_of(&s)?;
    if !lab.schema_files().iter().any(|(i, ..)| *i == id) {
        return Err(HttpError(404, format!("Schema not found: {id}")));
    }
    let raw = body
        .raw
        .ok_or_else(|| HttpError(400, "PUT needs {\"raw\": ...}".into()))?;
    json5::from_str::<Value>(&raw)
        .map_err(|e| HttpError(400, format!("settings are not valid JSON5: {e}")))?;
    let path = lab
        .user_settings_path(&id)
        .ok_or_else(|| HttpError(400, format!("bad settings id {id}")))?;
    write_atomic(&path, raw.as_bytes())?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- workspaces -----------------------------------------------------------

async fn list_workspaces(State(s): State<AppState>) -> HttpResult<Json<Value>> {
    let lab = lab_of(&s)?;
    let mut ids = Vec::new();
    let mut values = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&lab.workspaces_dir) {
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) != Some("jupyterlab-workspace") {
                continue;
            }
            let Some(ws) = read_json(&p) else { continue };
            let Some(id) = ws.pointer("/metadata/id").and_then(Value::as_str) else {
                continue;
            };
            ids.push(Value::from(id));
            values.push(ws);
        }
    }
    Ok(Json(json!({"workspaces": {"ids": ids, "values": values}})))
}

async fn get_workspace(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<Json<Value>> {
    let lab = lab_of(&s)?;
    Ok(Json(read_json(&lab.workspace_path(&id)).unwrap_or_else(
        || json!({"data": {}, "metadata": {"id": id}}),
    )))
}

async fn put_workspace(
    State(s): State<AppState>,
    Path(id): Path<String>,
    crate::routes::LenientJson(body): crate::routes::LenientJson<Value>,
) -> HttpResult<StatusCode> {
    let lab = lab_of(&s)?;
    let Value::Object(mut ws) = body else {
        return Err(HttpError(400, "workspace must be a JSON object".into()));
    };
    if !ws.get("data").map(Value::is_object).unwrap_or(false) {
        return Err(HttpError(400, "workspace needs a data object".into()));
    }
    let meta = ws
        .entry("metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| HttpError(400, "workspace metadata must be an object".into()))?;
    meta.insert("id".into(), id.clone().into());
    meta.insert("last_modified".into(), ganymede_api::now_iso().into());
    let bytes = serde_json::to_vec(&Value::Object(ws)).unwrap();
    write_atomic(&lab.workspace_path(&id), &bytes)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_workspace(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> HttpResult<StatusCode> {
    let lab = lab_of(&s)?;
    match std::fs::remove_file(lab.workspace_path(&id)) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(HttpError(404, format!("workspace {id} not found")))
        }
        Err(e) => Err(HttpError(500, e.to_string())),
    }
}

// ----- translations, listings, plugins, licenses -----------------------------

/// Language packs are installed as Python packages in `jupyterlab_server`;
/// none are supported, so English is the only language.
async fn list_translations() -> Json<Value> {
    Json(json!({
        "data": {"en": {"displayName": "English", "nativeName": "English"}},
        "message": "",
    }))
}

async fn get_translation(Path(locale): Path<String>) -> Json<Value> {
    let message = if locale == "en" || locale == "default" {
        String::new()
    } else {
        format!("Language pack '{locale}' not installed!")
    };
    Json(json!({"data": {}, "message": message}))
}

async fn listings() -> Json<Value> {
    Json(json!({
        "blocked_extensions_uris": [],
        "allowed_extensions_uris": [],
        "blocked_extensions": [],
        "allowed_extensions": [],
    }))
}

async fn plugins() -> Json<Value> {
    Json(json!({"lockRules": [], "allLocked": false}))
}

#[derive(Deserialize, Default)]
struct LicensesQuery {
    format: Option<String>,
    full_text: Option<String>,
}

/// The bundle's `third-party-licenses.json` in the JSON report shape.
/// Markdown and CSV reports are not implemented.
async fn licenses(
    State(s): State<AppState>,
    Query(q): Query<LicensesQuery>,
) -> HttpResult<Json<Value>> {
    let lab = lab_of(&s)?;
    if q.format.as_deref().unwrap_or("json") != "json" {
        return Err(HttpError(400, "only format=json is supported".into()));
    }
    let full = matches!(q.full_text.as_deref(), Some("true" | "1"));
    let mut packages = read_json(&lab.static_dir.join("third-party-licenses.json"))
        .and_then(|v| v.get("packages").cloned())
        .unwrap_or_else(|| json!([]));
    if !full {
        if let Some(list) = packages.as_array_mut() {
            for p in list {
                if let Some(o) = p.as_object_mut() {
                    o.remove("extractedText");
                }
            }
        }
    }
    let app_pkg = read_json(&lab.static_dir.join("package.json"))
        .and_then(|p| p.get("name").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| lab.app_name.clone());
    Ok(Json(json!({"bundles": {app_pkg: {"packages": packages}}})))
}

// ----- helpers --------------------------------------------------------------

fn read_json(p: &FsPath) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// No empty, `.` or `..` segments, and no backslashes.
fn safe_segments(p: &str) -> bool {
    !p.contains('\\') && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

fn write_atomic(path: &FsPath, bytes: &[u8]) -> HttpResult<()> {
    let io = |e: std::io::Error| HttpError(500, format!("writing {}: {e}", path.display()));
    let dir = path
        .parent()
        .ok_or_else(|| HttpError(500, "no parent directory".into()))?;
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    std::io::Write::write_all(&mut tmp, bytes).map_err(io)?;
    tmp.persist(path).map_err(|e| io(e.error))?;
    Ok(())
}

/// `{root}/{package}/{plugin}.json`, with scoped packages one level
/// deeper. The version comes from the package's `package.json.orig`.
fn schemas_under(root: &FsPath, default_version: &str) -> Vec<(String, PathBuf, String)> {
    let mut pkgs = Vec::new();
    for e in read_dir_sorted(root) {
        let name = file_name(&e);
        if name.starts_with('@') {
            for inner in read_dir_sorted(&e) {
                pkgs.push((format!("{name}/{}", file_name(&inner)), inner));
            }
        } else {
            pkgs.push((name, e));
        }
    }
    let mut out = Vec::new();
    for (pkg, dir) in pkgs {
        let version = read_json(&dir.join("package.json.orig"))
            .and_then(|p| p.get("version").and_then(Value::as_str).map(String::from))
            .unwrap_or_else(|| default_version.to_string());
        for f in read_dir_sorted(&dir) {
            let name = file_name(&f);
            if let Some(plugin) = name.strip_suffix(".json") {
                out.push((format!("{pkg}:{plugin}"), f.clone(), version.clone()));
            }
        }
    }
    out
}

fn read_dir_sorted(dir: &FsPath) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn file_name(p: &FsPath) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Prebuilt extensions: `{dir}/{name}/package.json` or
/// `{dir}/@scope/{name}/package.json` with a `jupyterlab._build` section.
/// The first directory that has a name wins, as in `jupyterlab_server`.
pub fn find_extensions(dirs: &[PathBuf]) -> Vec<FederatedExtension> {
    let mut out: Vec<FederatedExtension> = Vec::new();
    for root in dirs {
        let mut candidates = Vec::new();
        for e in read_dir_sorted(root) {
            if file_name(&e).starts_with('@') {
                candidates.extend(read_dir_sorted(&e));
            } else {
                candidates.push(e);
            }
        }
        for dir in candidates {
            let Some(pkg) = read_json(&dir.join("package.json")) else {
                continue;
            };
            let Some(name) = pkg.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(build) = pkg.pointer("/jupyterlab/_build").and_then(Value::as_object) else {
                continue;
            };
            if out.iter().any(|e| e.name == name) {
                continue;
            }
            out.push(FederatedExtension {
                name: name.to_string(),
                dir,
                build: build.clone(),
                version: pkg
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    out
}

// ----- installed bundles ------------------------------------------------

/// Where `ganymede lab install` keeps JupyterLab bundles, one directory per
/// version holding `static/`, `schemas/`, `themes/`, and `LICENSE`:
/// `$GANYMEDE_DATA_DIR/lab`, else `ganymede/lab` under the platform's local
/// data dir (`$XDG_DATA_HOME` or `~/.local/share` on Linux,
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows).
pub fn installs_dir() -> Option<PathBuf> {
    match std::env::var_os("GANYMEDE_DATA_DIR") {
        Some(d) if !d.is_empty() => Some(PathBuf::from(d).join("lab")),
        _ => dirs::data_local_dir().map(|d| d.join("ganymede").join("lab")),
    }
}

/// A JupyterLab bundle installed under [`installs_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    /// The application directory: `static/`, `schemas/`, `themes/`.
    pub dir: PathBuf,
}

impl Installed {
    pub fn static_dir(&self) -> PathBuf {
        self.dir.join("static")
    }
}

/// Bundles under `root`, newest version first. A directory counts once its
/// `static/index.html` exists; installs are renamed into place whole, and
/// names starting with `.` (unfinished installs) are skipped.
pub fn installed(root: &FsPath) -> Vec<Installed> {
    let mut out: Vec<Installed> = read_dir_sorted(root)
        .into_iter()
        .filter(|d| !file_name(d).starts_with('.') && d.join("static/index.html").is_file())
        .map(|dir| Installed {
            version: file_name(&dir),
            dir,
        })
        .collect();
    out.sort_by(|a, b| compare_versions(&b.version, &a.version));
    out
}

/// Orders versions such as `4.6.4`, `4.10.0`, and `4.7.0a2`: numeric
/// components compare as numbers, and a pre-release sorts before the
/// release it leads up to.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn key(v: &str) -> (Vec<u64>, bool, &str) {
        let split = v
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(v.len());
        let (nums, pre) = v.split_at(split);
        let nums = nums
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        (nums, pre.is_empty(), pre)
    }
    key(a).cmp(&key(b))
}

/// Renders the packaged `index.html`, which is a Jinja template, with the
/// handful of constructs it uses: `{# #}` comments, `{% set %}`,
/// `{% block %}`, `{% if custom_css %}`, and `{{ }}` for the page config,
/// `base_url`, and `page_config.fullStaticUrl`.
pub fn render_template(template: &str, config: &Value, base_url: &str) -> String {
    let mut out = String::with_capacity(template.len() + 4096);
    let mut rest = template;
    // Depth of `{% if %}` blocks being skipped; `custom_css` is never set.
    let mut skipping = 0usize;
    while let Some(start) = rest.find('{') {
        let (before, tail) = rest.split_at(start);
        if skipping == 0 {
            out.push_str(before);
        }
        let close = match tail.get(..2) {
            Some("{#") => "#}",
            Some("{%") => "%}",
            Some("{{") => "}}",
            _ => {
                if skipping == 0 {
                    out.push('{');
                }
                rest = &tail[1..];
                continue;
            }
        };
        let Some(end) = tail.find(close) else {
            out.push_str(tail);
            return out;
        };
        let inner = tail[2..end].trim();
        rest = &tail[end + 2..];
        match &tail[..2] {
            "{%" => {
                let word = inner.split_whitespace().next().unwrap_or("");
                match word {
                    "if" => skipping += 1,
                    "endif" => skipping = skipping.saturating_sub(1),
                    _ => {}
                }
            }
            "{{" if skipping == 0 => {
                let expr: String = inner.split_whitespace().collect();
                match expr.as_str() {
                    "page_config_full|tojson" | "page_config|tojson" => {
                        out.push_str(&script_safe_json(config))
                    }
                    "base_url|escape" | "base_url" => out.push_str(&html_escape(base_url)),
                    _ => {
                        let key = expr
                            .strip_prefix("page_config.")
                            .or_else(|| expr.strip_prefix("page_config_full."));
                        match key.and_then(|k| config.get(k)).and_then(Value::as_str) {
                            Some(v) => out.push_str(v),
                            None => tracing::warn!("index.html: unknown expression {expr}"),
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if skipping == 0 {
        out.push_str(rest);
    }
    out
}

/// JSON safe inside `<script>`, as Jinja's `tojson` makes it.
fn script_safe_json(v: &Value) -> String {
    serde_json::to_string(v)
        .unwrap()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\'', "\\u0027")
}

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_packaged_template_constructs() {
        let t = "<head>{# note #} {% set x = y.copy() %}<script id=\"jupyter-config-data\">{{ page_config_full | tojson }}</script>{% block favicon %}<link href=\"{{ base_url | escape }}static/f.ico\">{% endblock %} {% if custom_css %}<link href=\"{{ base_url }}custom.css\">{% endif %}<script src=\"{{page_config.fullStaticUrl}}/main.js\"></script>{}</head>";
        let html = render_template(
            t,
            &json!({"fullStaticUrl": "/b/static/lab", "x": "</script>"}),
            "/b/",
        );
        assert_eq!(
            html,
            "<head> <script id=\"jupyter-config-data\">{\"fullStaticUrl\":\"/b/static/lab\",\"x\":\"\\u003c/script\\u003e\"}</script><link href=\"/b/static/f.ico\"> <script src=\"/b/static/lab/main.js\"></script>{}</head>"
        );
    }

    #[test]
    fn orders_versions_numerically_with_prereleases_first() {
        let mut v = vec!["4.7.0a2", "4.10.0", "4.6.4", "4.7.0", "4.7.0rc1", "4.6.10"];
        v.sort_by(|a, b| compare_versions(a, b));
        assert_eq!(
            v,
            ["4.6.4", "4.6.10", "4.7.0a2", "4.7.0rc1", "4.7.0", "4.10.0"]
        );
    }

    #[test]
    fn lists_complete_installs_newest_first() {
        let root = tempfile::tempdir().unwrap();
        for v in ["4.6.4", "4.10.0", ".4.11.0.partial", "4.9.0"] {
            let s = root.path().join(v).join("static");
            std::fs::create_dir_all(&s).unwrap();
            if v != "4.9.0" {
                std::fs::write(s.join("index.html"), "").unwrap();
            }
        }
        let found: Vec<String> = installed(root.path())
            .into_iter()
            .map(|i| i.version)
            .collect();
        assert_eq!(found, ["4.10.0", "4.6.4"]);
        assert!(installed(&root.path().join("missing")).is_empty());
    }

    #[test]
    fn rejects_unsafe_settings_ids() {
        assert!(safe_segments("@jupyterlab/apputils-extension"));
        assert!(!safe_segments("@jupyterlab/../../etc"));
        assert!(!safe_segments("a//b"));
    }
}
