//! The REST and websocket surface against the fake kernel.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures::{SinkExt, StreamExt};
use ganymede_api::{
    KernelManager, KernelManagerConfig, SessionManager, StaticKernelspecs, TransportFactory,
};
use ganymede_contents::{FsContents, FsOptions};
use ganymede_kernel_host::protocol::{
    content::ExecuteRequest, ws::v1, Channel, ConnectionInfo, Message,
};
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    attach, in_process_pair, KernelTransport, Kernelspec, LaunchOptions, LaunchedKernel,
    Provisioner,
};
use ganymede_server::{build_router, AppState};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

struct FakeProvisioner;
#[async_trait]
impl Provisioner for FakeProvisioner {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(attach(ConnectionInfo::new_tcp(
            "127.0.0.1",
            [1, 2, 3, 4, 5],
        )))
    }
}
struct FakeTransports;
#[async_trait]
impl TransportFactory for FakeTransports {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(256);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

fn state(root: &std::path::Path) -> AppState {
    let spec = Kernelspec {
        name: "fake".into(),
        resource_dir: root.into(),
        argv: vec!["fake".into()],
        display_name: "Fake".into(),
        language: "fake".into(),
        interrupt_mode: ganymede_kernel_host::InterruptMode::Message,
        env: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    };
    let kernels = Arc::new(KernelManager::new(
        Arc::new(StaticKernelspecs(vec![spec])),
        Arc::new(FakeProvisioner),
        Arc::new(FakeTransports),
        KernelManagerConfig {
            startup_timeout: Duration::from_secs(5),
            root_dir: root.into(),
            ..Default::default()
        },
    ));
    AppState {
        sessions: Arc::new(SessionManager::new(kernels.clone())),
        kernels,
        contents: Arc::new(FsContents::new(root, FsOptions::default()).unwrap()),
        token: Some(TOKEN.into()),
        allow_origins: vec![],
        base_url: "/".into(),
        config_dir: root.join(".jupyter-config"),
        lab: None,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("token {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, value)
}

#[tokio::test]
async fn auth_is_required_and_token_query_works() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    assert_eq!(
        call(&app, "GET", "/api/status", None, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/status", None, Some("wrong"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/status", None, Some(TOKEN)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/status?token={TOKEN}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn kernelspecs_kernels_and_sessions_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    let (st, specs) = call(&app, "GET", "/api/kernelspecs", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(specs["default"], "fake");
    assert_eq!(specs["kernelspecs"]["fake"]["spec"]["display_name"], "Fake");
    assert_eq!(
        call(&app, "GET", "/api/kernelspecs/nope", None, t).await.0,
        StatusCode::NOT_FOUND
    );

    // JupyterLab's client sends JSON bodies with no Content-Type header.
    let req = Request::builder()
        .method("POST")
        .uri("/api/kernels")
        .header(header::AUTHORIZATION, format!("token {TOKEN}"))
        .body(Body::from(json!({"name": "fake"}).to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "JSON body without Content-Type is accepted"
    );
    let k: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    let id = k["id"].as_str().unwrap().to_string();
    assert_eq!(k["execution_state"], "idle");
    let (_, list) = call(&app, "GET", "/api/kernels", None, t).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/kernels/{id}/interrupt"),
            None,
            t
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (st, r) = call(&app, "POST", &format!("/api/kernels/{id}/restart"), None, t).await;
    assert_eq!((st, r["id"].as_str()), (StatusCode::OK, Some(id.as_str())));

    let (st, s) = call(
        &app,
        "POST",
        "/api/sessions",
        Some(json!({"path": "nb.ipynb", "type": "notebook", "kernel": {"id": id}})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{s}");
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["kernel"]["id"], id);
    assert_eq!(s["notebook"]["path"], "nb.ipynb");
    let (st, p) = call(
        &app,
        "PATCH",
        &format!("/api/sessions/{sid}"),
        Some(json!({"path": "renamed.ipynb"})),
        t,
    )
    .await;
    assert_eq!(
        (st, p["path"].as_str()),
        (StatusCode::OK, Some("renamed.ipynb"))
    );
    assert_eq!(
        call(&app, "DELETE", &format!("/api/sessions/{sid}"), None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", &format!("/api/kernels/{id}"), None, t)
            .await
            .0,
        StatusCode::NOT_FOUND,
        "session delete shuts the kernel down"
    );

    let (_, status) = call(&app, "GET", "/api/status", None, t).await;
    assert_eq!(status["kernels"], 0);
}

#[tokio::test]
async fn contents_rest_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    let (st, root) = call(&app, "GET", "/api/contents", None, t).await;
    assert_eq!(
        (st, root["type"].as_str()),
        (StatusCode::OK, Some("directory"))
    );
    assert_eq!(root["content"].as_array().unwrap().len(), 0);

    let (st, d) = call(
        &app,
        "PUT",
        "/api/contents/sub",
        Some(json!({"type": "directory"})),
        t,
    )
    .await;
    assert_eq!(
        (st, d["type"].as_str()),
        (StatusCode::CREATED, Some("directory")),
        "{d}"
    );

    let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code", "id": "c1", "source": "1+1", "metadata": {}, "outputs": [], "execution_count": null}]});
    let (st, saved) = call(
        &app,
        "PUT",
        "/api/contents/sub/a.ipynb",
        Some(json!({"type": "notebook", "format": "json", "content": nb})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{saved}");
    assert_eq!(saved["path"], "sub/a.ipynb");
    let (st, _) = call(
        &app,
        "PUT",
        "/api/contents/sub/a.ipynb",
        Some(json!({"type": "notebook", "format": "json", "content": nb})),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "second save is 200");

    let (st, got) = call(&app, "GET", "/api/contents/sub/a.ipynb", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(got["content"]["cells"][0]["source"], "1+1");
    let (_, meta) = call(&app, "GET", "/api/contents/sub/a.ipynb?content=0", None, t).await;
    assert!(meta["content"].is_null());

    let (st, cp) = call(
        &app,
        "POST",
        "/api/contents/sub/a.ipynb/checkpoints",
        None,
        t,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{cp}");
    let (_, cps) = call(
        &app,
        "GET",
        "/api/contents/sub/a.ipynb/checkpoints",
        None,
        t,
    )
    .await;
    assert_eq!(cps.as_array().unwrap().len(), 1);

    let (st, new) = call(
        &app,
        "POST",
        "/api/contents/sub",
        Some(json!({"type": "file", "ext": ".txt"})),
        t,
    )
    .await;
    assert_eq!(
        (st, new["path"].as_str()),
        (StatusCode::CREATED, Some("sub/untitled.txt"))
    );
    let (st, renamed) = call(
        &app,
        "PATCH",
        "/api/contents/sub/untitled.txt",
        Some(json!({"path": "sub/notes.txt"})),
        t,
    )
    .await;
    assert_eq!(
        (st, renamed["path"].as_str()),
        (StatusCode::OK, Some("sub/notes.txt"))
    );
    assert_eq!(
        call(&app, "DELETE", "/api/contents/sub/notes.txt", None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", "/api/contents/../etc", None, t).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/contents/missing.txt", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn websocket_channels_v1_execute_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let k = kernels.start(Some("fake"), None).await.unwrap();

    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as TMsg};
    let mut req = format!("ws://{addr}/api/kernels/{}/channels?session_id=abc", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", v1::SUBPROTOCOL.parse().unwrap());
    req.headers_mut()
        .insert("Authorization", format!("token {TOKEN}").parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(
        resp.headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok()),
        Some(v1::SUBPROTOCOL)
    );
    assert_eq!(kernels.model(&k.id).unwrap().connections, 1);

    let exec = Message::new(
        "execute_request",
        "client-sess",
        "me",
        &ExecuteRequest::new("ping"),
    );
    ws.send(TMsg::Binary(
        v1::encode(Channel::Shell, &exec).unwrap().into(),
    ))
    .await
    .unwrap();

    let mut got_reply = false;
    let mut got_idle = false;
    let mut stream_text = String::new();
    while !(got_reply && got_idle) {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("message in time")
            .unwrap()
            .unwrap();
        let TMsg::Binary(b) = frame else { continue };
        let (channel, msg) = v1::decode(&b).unwrap();
        if msg.parent_id() != Some(exec.msg_id()) {
            continue;
        }
        match (channel, msg.msg_type()) {
            (Channel::Shell, "execute_reply") => got_reply = true,
            (Channel::IoPub, "status") if msg.content["execution_state"] == "idle" => {
                got_idle = true
            }
            (Channel::IoPub, "stream") => {
                stream_text.push_str(msg.content["text"].as_str().unwrap())
            }
            _ => {}
        }
    }
    assert_eq!(stream_text, "echo: ping\n");

    // Shutting the kernel down closes the socket from the server side.
    kernels.shutdown(&k.id).await.unwrap();
    let mut closed = false;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if let TMsg::Close(_) = frame {
            closed = true;
            break;
        }
    }
    assert!(closed, "server sent a close frame after shutdown");
}

#[tokio::test]
async fn websocket_rejects_bad_token_and_foreign_origin() {
    let dir = tempfile::tempdir().unwrap();
    let st = state(dir.path());
    let kernels = st.kernels.clone();
    let app = build_router(st);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let k = kernels.start(Some("fake"), None).await.unwrap();

    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let req = format!("ws://{addr}/api/kernels/{}/channels", k.id)
        .into_client_request()
        .unwrap();
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "no token"
    );

    let mut req = format!("ws://{addr}/api/kernels/{}/channels?token={TOKEN}", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", "http://evil.example".parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "foreign origin"
    );

    let mut req = format!("ws://{addr}/api/kernels/{}/channels?token={TOKEN}", k.id)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Origin", format!("http://{addr}").parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_ok(),
        "same-host origin with query token"
    );
}

#[tokio::test]
async fn server_info_identity_and_config() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);

    // `GET /api` is public in jupyter_server.
    let (st, v) = call(&app, "GET", "/api", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["version"].as_str().unwrap().starts_with("2."));

    let (st, me) = call(&app, "GET", "/api/me", None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert!(me["identity"]["username"].is_string());
    assert_eq!(
        call(&app, "GET", "/api/me", None, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (st, c) = call(&app, "GET", "/api/config/notebook", None, t).await;
    assert_eq!((st, c), (StatusCode::OK, json!({})));
    let (_, c) = call(
        &app,
        "PATCH",
        "/api/config/notebook",
        Some(json!({"a": {"b": 1, "c": 2}, "d": 3})),
        t,
    )
    .await;
    assert_eq!(c, json!({"a": {"b": 1, "c": 2}, "d": 3}));
    let (_, c) = call(
        &app,
        "PATCH",
        "/api/config/notebook",
        Some(json!({"a": {"c": null}, "d": null})),
        t,
    )
    .await;
    assert_eq!(c, json!({"a": {"b": 1}}));
    let (_, c) = call(&app, "GET", "/api/config/notebook", None, t).await;
    assert_eq!(c, json!({"a": {"b": 1}}), "persisted");
    assert_eq!(
        call(&app, "GET", "/api/config/..%2Fescape", None, t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    let (st, nb) = call(&app, "GET", "/api/nbconvert", None, t).await;
    assert_eq!((st, nb), (StatusCode::OK, json!({})));
}

#[tokio::test]
async fn kernelspec_resources_are_listed_and_served() {
    let dir = tempfile::tempdir().unwrap();
    // The fake spec's resource dir is the root.
    std::fs::write(dir.path().join("logo-64x64.png"), b"png").unwrap();
    std::fs::write(dir.path().join("kernel.json"), b"{}").unwrap();
    let app = build_router(state(dir.path()));
    let t = Some(TOKEN);
    let (_, specs) = call(&app, "GET", "/api/kernelspecs", None, t).await;
    assert_eq!(
        specs["kernelspecs"]["fake"]["resources"],
        json!({"logo-64x64": "/kernelspecs/fake/logo-64x64.png"})
    );
    let (st, body) = call(&app, "GET", "/kernelspecs/fake/logo-64x64.png", None, t).await;
    assert_eq!((st, body), (StatusCode::OK, json!("png")));
    assert_eq!(
        call(&app, "GET", "/kernelspecs/fake/missing.png", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", "/kernelspecs/nope/logo-64x64.png", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn events_websocket_accepts_authenticated_clients() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state(dir.path()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/events/subscribe"))
            .await
            .is_err(),
        "no token"
    );
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/events/subscribe?token={TOKEN}"))
            .await
            .unwrap();
    ws.close(None).await.unwrap();
}

// ----- JupyterLab --------------------------------------------------------------

/// A miniature JupyterLab application directory with the packaged
/// template's constructs, one core schema, one theme, and one prebuilt
/// extension with its own schema.
fn fake_lab(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let app = root.join("app");
    let w = |p: std::path::PathBuf, s: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    };
    w(
        app.join("static/index.html"),
        "<html><head>{# c #} {% set _ = x %}<script id=\"jupyter-config-data\" type=\"application/json\">{{ page_config_full | tojson }}</script>{% block favicon %}<link href=\"{{ base_url | escape }}static/favicons/favicon.ico\">{% endblock %}<script src=\"{{page_config.fullStaticUrl}}/main.js\"></script></head></html>",
    );
    w(
        app.join("static/package.json"),
        r#"{"name": "@jupyterlab/application-top", "jupyterlab": {"name": "JupyterLab", "version": "4.9.9"}}"#,
    );
    w(app.join("static/main.js"), "main");
    w(
        app.join("static/third-party-licenses.json"),
        r#"{"packages": [{"name": "x", "extractedText": "long"}]}"#,
    );
    w(
        app.join("schemas/@jupyterlab/apputils-extension/themes.json"),
        r#"{"title": "Theme", "properties": {"theme": {"type": "string", "default": "JupyterLab Light"}}}"#,
    );
    w(
        app.join("schemas/@jupyterlab/apputils-extension/package.json.orig"),
        r#"{"version": "4.9.8"}"#,
    );
    w(
        app.join("themes/@jupyterlab/theme-light-extension/index.css"),
        "body{}",
    );
    let ext = root.join("labextensions");
    w(
        ext.join("my_ext/package.json"),
        r#"{"name": "my_ext", "version": "0.1.0", "jupyterlab": {"_build": {"load": "static/remoteEntry.abc.js", "extension": "./extension"}}}"#,
    );
    w(ext.join("my_ext/static/remoteEntry.abc.js"), "remote");
    w(
        ext.join("my_ext/schemas/my_ext/plugin.json"),
        r#"{"title": "Mine"}"#,
    );
    (app.join("static"), ext)
}

fn lab_state(root: &std::path::Path) -> AppState {
    let (static_dir, ext) = fake_lab(root);
    let mut st = state(root);
    st.lab =
        ganymede_server::lab::Lab::detect(&static_dir, &[ext], &st.config_dir, root).map(Arc::new);
    assert!(st.lab.is_some(), "fake app dir is recognised");
    st
}

async fn get_text(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn lab_page_carries_page_config_and_assets_are_public() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));

    let resp = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.headers()[header::LOCATION], "/lab");

    assert_eq!(get_text(&app, "/lab").await.0, StatusCode::FORBIDDEN);
    let (st, html) = get_text(&app, &format!("/lab/tree/a/b.ipynb?token={TOKEN}")).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        html.contains("<script src=\"/static/lab/main.js\">"),
        "{html}"
    );
    assert!(html.contains("<link href=\"/static/favicons/favicon.ico\">"));
    assert!(!html.contains("{%") && !html.contains("{{") && !html.contains("{#"));
    let json = html
        .split("type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let cfg: Value = serde_json::from_str(json).unwrap();
    assert_eq!(cfg["appVersion"], "4.9.9");
    assert_eq!(cfg["token"], TOKEN);
    assert_eq!(cfg["treePath"], "a/b.ipynb");
    assert_eq!(cfg["terminalsAvailable"], false);
    assert_eq!(
        cfg["federated_extensions"],
        json!([{"name": "my_ext", "load": "static/remoteEntry.abc.js", "entrypoints": null, "extension": "./extension"}])
    );
    let (_, html) = get_text(&app, &format!("/lab/workspaces/w1?token={TOKEN}")).await;
    assert!(html.contains("\"workspace\":\"w1\""));

    assert_eq!(
        get_text(&app, "/static/lab/main.js").await,
        (StatusCode::OK, "main".into())
    );
    assert_eq!(
        get_text(
            &app,
            "/lab/api/themes/@jupyterlab/theme-light-extension/index.css"
        )
        .await,
        (StatusCode::OK, "body{}".into())
    );
    assert_eq!(
        get_text(&app, "/lab/extensions/my_ext/static/remoteEntry.abc.js").await,
        (StatusCode::OK, "remote".into())
    );
    assert_eq!(
        get_text(&app, "/lab/api/themes/../static/main.js").await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn lab_settings_round_trip_json5() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));
    let t = Some(TOKEN);
    assert_eq!(
        call(&app, "GET", "/lab/api/settings", None, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (_, ids) = call(&app, "GET", "/lab/api/settings?ids_only=true", None, t).await;
    assert_eq!(
        ids,
        json!({"settings": [{"id": "@jupyterlab/apputils-extension:themes"}, {"id": "my_ext:plugin"}]})
    );

    let id = "@jupyterlab/apputils-extension:themes";
    let (st, s) = call(&app, "GET", &format!("/lab/api/settings/{id}"), None, t).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["raw"], "{}");
    assert_eq!(s["version"], "4.9.8");
    assert_eq!(s["schema"]["title"], "Theme");

    let raw = "{\n  // a comment\n  \"theme\": \"JupyterLab Dark\",\n}";
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/lab/api/settings/{id}"),
        Some(json!({ "raw": raw })),
        t,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, s) = call(&app, "GET", &format!("/lab/api/settings/{id}"), None, t).await;
    assert_eq!(s["raw"], raw);
    assert_eq!(s["settings"], json!({"theme": "JupyterLab Dark"}));
    assert!(s["last_modified"].is_string());

    let (_, all) = call(&app, "GET", "/lab/api/settings", None, t).await;
    assert_eq!(all["settings"][0]["settings"]["theme"], "JupyterLab Dark");
    assert_eq!(all["settings"][1]["version"], "0.1.0");

    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/lab/api/settings/{id}"),
            Some(json!({"raw": "{nope"})),
            t
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "GET", "/lab/api/settings/nope:nope", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn lab_workspaces_translations_and_listings() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(lab_state(dir.path()));
    let t = Some(TOKEN);

    let (_, ws) = call(&app, "GET", "/lab/api/workspaces/default", None, t).await;
    assert_eq!(ws, json!({"data": {}, "metadata": {"id": "default"}}));
    let body =
        json!({"data": {"layout-restorer:data": {"main": {}}}, "metadata": {"id": "default"}});
    assert_eq!(
        call(&app, "PUT", "/lab/api/workspaces/default", Some(body), t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, ws) = call(&app, "GET", "/lab/api/workspaces/default", None, t).await;
    assert_eq!(ws["data"]["layout-restorer:data"], json!({"main": {}}));
    let (_, list) = call(&app, "GET", "/lab/api/workspaces", None, t).await;
    assert_eq!(list["workspaces"]["ids"], json!(["default"]));
    assert_eq!(
        call(&app, "PUT", "/lab/api/workspaces/x", Some(json!({})), t)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&app, "DELETE", "/lab/api/workspaces/default", None, t)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "DELETE", "/lab/api/workspaces/default", None, t)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    let (_, tr) = call(&app, "GET", "/lab/api/translations", None, t).await;
    assert_eq!(tr["data"]["en"]["displayName"], "English");
    let (_, tr) = call(&app, "GET", "/lab/api/translations/default", None, t).await;
    assert_eq!(tr, json!({"data": {}, "message": ""}));

    let (st, l) = call(
        &app,
        "GET",
        "/lab/api/listings/@jupyterlab/extensionmanager-extension/listings.json",
        None,
        t,
    )
    .await;
    assert_eq!(
        (st, l["blocked_extensions"].clone()),
        (StatusCode::OK, json!([]))
    );
    let (_, p) = call(&app, "GET", "/lab/api/plugins", None, t).await;
    assert_eq!(p, json!({"lockRules": [], "allLocked": false}));
    let (_, lic) = call(&app, "GET", "/lab/api/licenses", None, t).await;
    assert_eq!(
        lic,
        json!({"bundles": {"@jupyterlab/application-top": {"packages": [{"name": "x"}]}}})
    );
}
