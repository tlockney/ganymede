//! A connected machine mounted under `/machines/{id}/` (#894): a local
//! Ganymede server forwards an allowlist of routes to another one (kernels
//! are in-process fakes), or to a stand-in that records what it was sent.

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use ganymede_gateway::mount::Machines;
use ganymede_kernel_host::protocol::{
    content::ExecuteRequest, ws::v1, Channel, Message as KernelMessage,
};
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    in_process, in_process_pair, KernelTransport, Kernelspec, KernelspecSource, LaunchOptions,
    LaunchedKernel, Provisioner, TransportFactory,
};
use ganymede_server::config::Config;
use ganymede_server::serve::{run_with, Startup};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMsg;

const LOCAL_TOKEN: &str = "local-token";
const MACHINE_TOKEN: &str = "machine-token";

/// The machine's kernels.
struct Fakes;

impl KernelspecSource for Fakes {
    fn list(&self) -> ganymede_kernel_host::Result<Vec<Kernelspec>> {
        let mut spec: Kernelspec = serde_json::from_value(json!({
            "argv": [],
            "display_name": "Machine fake",
            "language": "fake",
        }))?;
        spec.name = "fake".into();
        Ok(vec![spec])
    }
}

#[async_trait]
impl Provisioner for Fakes {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(in_process())
    }
}

#[async_trait]
impl TransportFactory for Fakes {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

/// A running Ganymede server: its `host:port`, the registry its app keeps,
/// and its stopper.
struct Server {
    addr: String,
    token: String,
    machines: Arc<Machines>,
    _stop: tokio::sync::mpsc::Sender<()>,
    root: tempfile::TempDir,
}

fn config(root: &std::path::Path, token: Option<&str>) -> Config {
    Config {
        port: 0,
        token: token.map(String::from),
        root_dir: root.into(),
        cookie_secret_file: Some(root.join("cookie")),
        notary_dir: Some(root.join("notary")),
        ..Default::default()
    }
}

async fn start(adjust: impl FnOnce(&mut Config)) -> Server {
    start_with(adjust, Machines::new()).await
}

async fn start_with(adjust: impl FnOnce(&mut Config), machines: Machines) -> Server {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let mut cfg = config(&work, Some(LOCAL_TOKEN));
    cfg.cookie_secret_file = Some(root.path().join("cookie"));
    cfg.notary_dir = Some(root.path().join("notary"));
    cfg.terminal_shell = vec!["/bin/sh".into()];
    adjust(&mut cfg);
    let token = cfg.token.clone();
    let (url_tx, url_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    let machines = Arc::new(machines);
    let fakes = Arc::new(Fakes);
    tokio::spawn(run_with(
        cfg,
        Startup {
            on_listening: Some(Box::new(move |url: &str| {
                let _ = url_tx.send(url.to_string());
            })),
            stop: Some(stop_rx),
            kernelspecs: Some(fakes.clone()),
            provisioner: Some(fakes.clone()),
            transports: Some(fakes),
            machines: Some(machines.clone()),
            ..Default::default()
        },
    ));
    let url = url_rx.await.unwrap();
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();
    Server {
        addr: addr.to_string(),
        token: token.unwrap_or_default(),
        machines,
        _stop: stop_tx,
        root,
    }
}

impl Server {
    fn work(&self) -> std::path::PathBuf {
        self.root.path().join("work")
    }

    fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// Mounts `machine` (a server of its own) as `id`, with its token.
    fn mount(&self, id: &str, machine: &Server) {
        self.machines
            .add(id, &machine.url(), Some(MACHINE_TOKEN))
            .unwrap();
    }
}

/// A machine: a Ganymede server with its own token.
async fn machine() -> Server {
    start(|cfg| cfg.token = Some(MACHINE_TOKEN.into())).await
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_backend_preconfigured(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth(),
        )
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// A request to `path` on `server` with the local token.
fn get(server: &Server, path: &str) -> reqwest::RequestBuilder {
    request(server, reqwest::Method::GET, path)
}

fn request(server: &Server, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    client()
        .request(method, format!("http://{}{path}", server.addr))
        .header("Authorization", format!("token {}", server.token))
}

/// The login cookie (`name=value`) the local server gives for its token.
async fn login_cookie(server: &Server) -> String {
    let resp = get(server, "/api/status").send().await.unwrap();
    let set = resp.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

/// The status of a GET of `path` exactly as written, with the token.
async fn raw_status(server: &Server, path: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(&server.addr).await.unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: token {}\r\nConnection: close\r\n\r\n",
        server.addr, server.token
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out[9..12].parse().unwrap()
}

// ----- a stand-in machine that records what it is sent -------------------------------

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    uri: String,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

async fn record(State(log): State<Log>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 1 << 20).await.unwrap().to_vec();
    let path = parts.uri.path().to_string();
    log.lock().unwrap().push(Seen {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        headers: parts.headers,
        body,
    });
    let html = [
        ("content-type", "text/html"),
        ("content-security-policy", "default-src *"),
        ("x-content-type-options", "nosniff"),
        ("set-cookie", "machine_session=1; Path=/"),
        ("set-cookie", "_xsrf=2; Path=/"),
    ];
    // Redirects out of the mount, and one within it.
    let redirect = |location: &'static str| {
        (
            axum::http::StatusCode::TEMPORARY_REDIRECT,
            [("location", location)],
        )
            .into_response()
    };
    match path.as_str() {
        "/api/contents/redir-evil" => return redirect("/../../api/contents/x"),
        "/api/contents/redir-enc" => return redirect("/%2e%2e/%2e%2e/api/contents/x"),
        "/api/contents/redir-ok" => return redirect("/api/contents/y?x=1"),
        // Accepts the request and never answers.
        "/api/contents/hang" => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        _ => {}
    }
    if path == "/nbconvert/html/bare.ipynb" {
        // No policy of its own, and a login cookie.
        return (
            [
                ("content-type", "text/html"),
                ("set-cookie", "username-machine=login; Path=/"),
            ],
            "<p>page</p>",
        )
            .into_response();
    }
    if path.starts_with("/files/") || path.starts_with("/nbconvert/") {
        return (
            [html[0], html[1], html[2], html[3], html[4]],
            "<script>alert(1)</script>",
        )
            .into_response();
    }
    (
        [("set-cookie", "machine_session=1; Path=/")],
        axum::Json(json!({"ok": true})),
    )
        .into_response()
}

/// Its `host:port` and what it has been sent.
async fn stand_in() -> (String, Log) {
    let log = Log::default();
    let app = axum::Router::new().fallback(record).with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, log)
}

fn seen(log: &Log) -> Vec<Seen> {
    log.lock().unwrap().clone()
}

// ----- who may use the mount ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_tailnet_identity_is_refused() {
    let (addr, log) = stand_in().await;
    let local = start(|cfg| cfg.tailscale.allow = vec!["me@example.com".into()]).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    // The setup lets this identity into the server's own API: a program
    // on an allowed login, as `tailscale serve` names it.
    let identity = |path: &str| {
        client()
            .get(format!("http://{}{path}", local.addr))
            .header("Tailscale-User-Login", "me@example.com")
            .header("Ganymede-Client", "an-app")
    };
    assert_eq!(identity("/api/status").send().await.unwrap().status(), 200);
    // ...and not into the machine behind it.
    for path in ["/machines/m/api/status", "/machines/m/api/contents"] {
        let resp = identity(path).send().await.unwrap();
        assert_eq!(resp.status(), 403, "{path}");
    }
    assert!(seen(&log).is_empty(), "{:?}", seen(&log));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_token_is_refused() {
    let (addr, log) = stand_in().await;
    let local = start(|cfg| {
        cfg.mcp_enabled = true;
        cfg.mcp_token = Some("agent-token".into());
    })
    .await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    for header in ["token agent-token", "Bearer agent-token"] {
        let resp = client()
            .get(format!("http://{}/machines/m/api/status", local.addr))
            .header("Authorization", header)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{header}");
    }
    let resp = client()
        .get(format!(
            "http://{}/machines/m/api/status?token=agent-token",
            local.addr
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert!(seen(&log).is_empty());
    // The local token does work, so the refusals above are about the token.
    assert_eq!(
        get(&local, "/machines/m/api/status")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_credentials_is_refused() {
    let (addr, log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let anonymous =
        |method, path: &str| client().request(method, format!("http://{}{path}", local.addr));
    for (method, path) in [
        (reqwest::Method::GET, "/machines/m/api/status"),
        (reqwest::Method::PUT, "/machines/m/api/contents/a.txt"),
        (reqwest::Method::GET, "/machines/nobody/api/status"),
    ] {
        let resp = anonymous(method, path).send().await.unwrap();
        assert_eq!(resp.status(), 403, "{path}");
    }
    let resp = anonymous(reqwest::Method::GET, "/machines/m/api/status")
        .header("Authorization", "token wrong")
        .header("Cookie", "username-x=forged")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert!(seen(&log).is_empty());

    // With auth off nobody is the page's own session: nothing is mounted
    // for them either.
    let open = start(|cfg| cfg.token = Some(String::new())).await;
    open.machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let resp = client()
        .get(format!("http://{}/machines/m/api/status", open.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert!(seen(&log).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_session_by_cookie_may_use_it_and_changes_need_xsrf() {
    let (addr, log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let cookie = login_cookie(&local).await;
    let url = |p: &str| format!("http://{}{p}", local.addr);
    let origin = format!("http://{}", local.addr);
    let resp = client()
        .get(url("/machines/m/api/status"))
        .header("Cookie", &cookie)
        .header("Origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    // A change without the XSRF token is refused here, not forwarded.
    let before = seen(&log).len();
    let resp = client()
        .put(url("/machines/m/api/contents/a.txt"))
        .header("Cookie", &cookie)
        .header("Origin", &origin)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    // Another site's page with the cookie is refused too.
    let resp = client()
        .get(url("/machines/m/api/status"))
        .header("Cookie", &cookie)
        .header("Origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(seen(&log).len(), before);
}

// ----- what is forwarded -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn only_the_allowlist_is_forwarded() {
    let (addr, log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    // Each of these reaches the stand-in.
    for path in [
        "api/status",
        "api/me",
        "api/contents/a.ipynb",
        "files/a.png",
        "api/collaboration/session/a.ipynb",
        "api/fileid/id?path=a",
        "api/kernels",
        "api/sessions",
        "api/kernelspecs",
        "api/terminals",
        "api/metrics/v1/kernel_usage/get_usage/k",
        "api/events/subscribe",
        "nbconvert/html/a.ipynb",
    ] {
        let resp = get(&local, &format!("/machines/m/{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    let n = seen(&log).len();
    assert_eq!(n, 13);
    // None of these does.
    for path in [
        "mcp",
        "mcp/",
        "api/host/grants/a.ipynb",
        "api/config/x",
        "api/",
        "lab",
        "login",
        "lsp/status",
        "api/status/extra",
    ] {
        let resp = get(&local, &format!("/machines/m/{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "{path}");
    }
    // Nor does a path that climbs out of an allowed one, however spelled.
    for path in [
        "api/contents/../../mcp",
        "api/contents/%2e%2e/%2e%2e/mcp",
        "api/contents/%2E%2e/api/host/grants/x",
        "api/contents/..%2f..%2fmcp",
        "api/contents/a%2Fb",
        "api/contents/a%5cb",
        "files/%2e%2e%2f%2e%2e%2fetc/passwd",
        "api%2fhost/grants/x",
        "api//contents",
        "api/contents/%00",
    ] {
        // Sent as written: an HTTP client would tidy `..` away itself.
        assert_eq!(
            raw_status(&local, &format!("/machines/m/{path}")).await,
            400,
            "{path}"
        );
    }
    assert_eq!(seen(&log).len(), n, "{:?}", seen(&log));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pages_credentials_are_not_forwarded_and_the_machines_are_added() {
    let (addr, log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/base/"), Some("machine-secret"))
        .unwrap();
    let cookie = login_cookie(&local).await;
    let origin = format!("http://{}", local.addr);
    // The page: its cookie, its Origin, an XSRF token, the local token in
    // both places, and a Ganymede-Client of its own.
    let resp = client()
        .post(format!(
            "http://{}/machines/m/api/contents/a.txt?a=1&token={LOCAL_TOKEN}&_xsrf=x&b=%20c",
            local.addr
        ))
        .header("Cookie", format!("{cookie}; _xsrf=x"))
        .header("Origin", &origin)
        .header("Referer", format!("{origin}/lab"))
        .header("X-XSRFToken", "x")
        .header("Sec-Fetch-Site", "same-origin")
        .header("Ganymede-Client", "the-page")
        .header("X-Forwarded-For", "1.2.3.4")
        .header("Tailscale-User-Login", "me@example.com")
        .header("Content-Type", "application/json")
        .header("If-Match", "\"e\"")
        .body(r#"{"k":1}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let all = seen(&log);
    let [one] = &all[..] else { panic!("{all:?}") };
    assert_eq!(one.method, "POST");
    assert_eq!(one.uri, "/base/api/contents/a.txt?a=1&b=%20c");
    assert_eq!(one.body, br#"{"k":1}"#);
    let h = &one.headers;
    assert_eq!(h["authorization"], "token machine-secret");
    assert_eq!(h["ganymede-client"], "ganymede-mount");
    assert_eq!(h["content-type"], "application/json");
    assert_eq!(h["if-match"], "\"e\"");
    for gone in [
        "cookie",
        "origin",
        "referer",
        "x-xsrftoken",
        "sec-fetch-site",
        "x-forwarded-for",
        "tailscale-user-login",
    ] {
        assert!(h.get(gone).is_none(), "{gone}: {h:?}");
    }
    let text = format!("{h:?}{}", one.uri);
    assert!(
        !text.contains(LOCAL_TOKEN) && !text.contains("username-"),
        "{text}"
    );

    // The local token in the Authorization header is not forwarded either,
    // and a machine with no token gets only the client header.
    local
        .machines
        .add("bare", &format!("http://{addr}/"), None)
        .unwrap();
    let resp = get(&local, "/machines/bare/api/status")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let h = &seen(&log)[1].headers;
    assert!(h.get("authorization").is_none(), "{h:?}");
    assert_eq!(h["ganymede-client"], "ganymede-mount");
}

#[tokio::test(flavor = "multi_thread")]
async fn set_cookie_is_dropped_and_the_files_policy_replaced() {
    let (addr, _log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();

    let resp = get(&local, "/machines/m/api/status").send().await.unwrap();
    // The local token authenticated the request, so the *local* server
    // sets its own login cookie; the machine's is gone.
    let cookies: Vec<_> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies.iter().all(|c| !c.contains("machine_session")),
        "{cookies:?}"
    );
    let cookie = login_cookie(&local).await;
    let resp = client()
        .get(format!("http://{}/machines/m/files/page.html", local.addr))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let h = resp.headers();
    assert!(h.get("set-cookie").is_none(), "{h:?}");
    // Not the machine's `default-src *`, and only one policy.
    let csp: Vec<_> = h.get_all("content-security-policy").iter().collect();
    assert_eq!(csp.len(), 1, "{h:?}");
    // What this server's own `/files` sends: the sandbox, and its
    // frame-ancestors.
    assert_eq!(
        csp[0], "sandbox allow-scripts; frame-ancestors 'self'",
        "{h:?}"
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-type"], "text/html");
    assert_eq!(resp.text().await.unwrap(), "<script>alert(1)</script>");

    // An export is a page too.
    let resp = get(&local, "/machines/m/nbconvert/html/a.ipynb")
        .send()
        .await
        .unwrap();
    let csp: Vec<_> = resp
        .headers()
        .get_all("content-security-policy")
        .iter()
        .collect();
    assert_eq!(
        csp,
        ["sandbox allow-scripts allow-downloads; frame-ancestors 'self'"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_response_gets_the_sandbox_policy_and_no_cookie() {
    let (addr, _log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let cookie = login_cookie(&local).await;
    let own = "sandbox allow-scripts; frame-ancestors 'self'";
    let page = "sandbox allow-scripts allow-downloads; frame-ancestors 'self'";
    for (path, want) in [
        ("api/status", own),
        ("api/contents/a.ipynb", own),
        ("api/kernelspecs", own),
        ("files/page.html", own),
        ("nbconvert/script/a.ipynb", own),
        ("nbconvert/html/a.ipynb", page),
        // The machine sent no policy at all, and a login cookie.
        ("nbconvert/html/bare.ipynb", page),
    ] {
        let resp = client()
            .get(format!("http://{}/machines/m/{path}", local.addr))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        let h = resp.headers();
        let csp: Vec<_> = h.get_all("content-security-policy").iter().collect();
        assert_eq!(csp, [want], "{path}: {h:?}");
        assert!(h.get("set-cookie").is_none(), "{path}: {h:?}");
    }
}

/// A client that follows redirects, as a page's `fetch` does for a 307.
fn following_client() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_backend_preconfigured(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth(),
        )
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap()
}

/// A machine's redirect can't send the page's PUT or DELETE (a 307 repeats
/// the method, body and credentials) to this server's own API, on a
/// server at the root or under a base URL, and one within the mount keeps
/// the mount's prefix, base URL included.
async fn redirects_stay_in_the_mount(base: &str) {
    let (addr, log) = stand_in().await;
    let base_url = format!("{base}/");
    let local = start(|cfg| cfg.base_url = base_url.clone()).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let url = |p: &str| format!("http://{}{base}/machines/m/api/contents/{p}", local.addr);
    let auth = format!("token {}", local.token);
    for method in [reqwest::Method::PUT, reqwest::Method::DELETE] {
        for name in ["redir-evil", "redir-enc"] {
            let resp = following_client()
                .request(method.clone(), url(name))
                .header("Authorization", &auth)
                .body(r#"{"type":"file","format":"text","content":"pwned"}"#)
                .send()
                .await
                .unwrap();
            // The redirect is the final answer, with nowhere to go.
            assert_eq!(resp.status(), 307, "{method} {name}");
            assert!(resp.headers().get("location").is_none(), "{method} {name}");
        }
    }
    // Only the four requests to the machine; none reached this server's API.
    assert_eq!(seen(&log).len(), 4);
    assert!(!local.work().join("x").exists());
    assert!(local.work().read_dir().unwrap().next().is_none());
    // Within the mount: rewritten under the prefix, base URL included.
    let resp = request(
        &local,
        reqwest::Method::PUT,
        &format!("{base}/machines/m/api/contents/redir-ok"),
    )
    .body("{}")
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 307);
    assert_eq!(
        resp.headers()["location"],
        format!("{base}/machines/m/api/contents/y?x=1").as_str()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_cannot_leave_the_mount() {
    redirects_stay_in_the_mount("").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_cannot_leave_the_mount_under_a_base_url() {
    redirects_stay_in_the_mount("/base").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_that_never_answers_is_a_504() {
    let (addr, _log) = stand_in().await;
    let local = start_with(
        |_| {},
        Machines::with_response_timeout(Duration::from_millis(300)),
    )
    .await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    let started = std::time::Instant::now();
    let resp = get(&local, "/machines/m/api/contents/hang")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["message"].as_str().unwrap().contains("did not answer"),
        "{body}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    // The machine still answers other requests.
    assert_eq!(
        get(&local, "/machines/m/api/status")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn other_methods_are_not_forwarded() {
    let (addr, log) = stand_in().await;
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();
    for method in [reqwest::Method::OPTIONS, reqwest::Method::TRACE] {
        let resp = request(&local, method.clone(), "/machines/m/api/status")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 405, "{method}");
    }
    assert!(seen(&log).is_empty(), "{:?}", seen(&log));
}

/// Each direction of a proxied socket is forwarded on its own: a machine
/// that floods a page that isn't reading doesn't keep the page's own
/// frames from reaching the machine.
#[tokio::test(flavor = "multi_thread")]
async fn a_flood_to_a_page_that_does_not_read_does_not_block_the_other_direction() {
    use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
    let (got_tx, mut got_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    async fn flood(
        ws: WebSocketUpgrade,
        State(tx): State<tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Response {
        ws.on_upgrade(move |socket: WebSocket| async move {
            let (mut sink, mut stream) = socket.split();
            // Sends until the socket is gone.
            tokio::spawn(async move {
                let chunk = vec![7u8; 256 * 1024];
                while sink
                    .send(Message::Binary(chunk.clone().into()))
                    .await
                    .is_ok()
                {}
            });
            while let Some(Ok(msg)) = stream.next().await {
                if let Message::Text(t) = msg {
                    let _ = tx.send(t.as_str().to_string());
                }
            }
        })
    }
    let app = axum::Router::new().fallback(flood).with_state(got_tx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let local = start(|_| {}).await;
    local
        .machines
        .add("m", &format!("http://{addr}/"), None)
        .unwrap();

    // The page connects and reads nothing while the machine floods it.
    let (mut ws, _) = ws_to(&local, "/machines/m/api/events/subscribe", None).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    ws.send(WsMsg::Text("from the page".into())).await.unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(5), got_rx.recv()).await;
    assert_eq!(heard.ok().flatten().as_deref(), Some("from the page"));
}

// ----- lifecycle ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn machines_are_added_and_removed_at_run_time() {
    let mini = machine().await;
    std::fs::write(mini.work().join("a.txt"), "on the mini").unwrap();
    let local = start(|_| {}).await;
    let status = |id: &'static str| {
        let local = &local;
        async move {
            get(local, &format!("/machines/{id}/api/status"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    // Nothing is mounted until it is added.
    assert_eq!(status("mini").await, 404);
    let body = get(&local, "/machines/mini/api/status")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["message"].is_string(),
        "{body}"
    );
    local.mount("mini", &mini);
    assert_eq!(status("mini").await, 200);
    assert_eq!(local.machines.ids(), ["mini"]);
    assert!(local.machines.remove("mini"));
    assert_eq!(status("mini").await, 404);
    assert!(!local.machines.remove("mini"));
    local.mount("mini", &mini);
    assert_eq!(status("mini").await, 200);
    // Another id is another machine.
    assert_eq!(status("other").await, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_that_is_down_is_a_502() {
    // A port nobody listens on.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let local = start(|_| {}).await;
    local
        .machines
        .add("gone", &format!("http://{addr}/"), Some("secret-token"))
        .unwrap();
    let resp = get(&local, "/machines/gone/api/contents")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 502);
    let body: Value = resp.json().await.unwrap();
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("gone") && message.contains("unreachable"),
        "{body}"
    );
    assert!(!message.contains("secret-token"), "{body}");
    // The websocket is refused at the handshake, not opened and closed.
    let mut req = format!("ws://{}/machines/gone/api/events/subscribe", local.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "Authorization",
        format!("token {LOCAL_TOKEN}").parse().unwrap(),
    );
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 502),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_machines_stay_apart() {
    let mini = machine().await;
    let laptop = machine().await;
    std::fs::write(mini.work().join("who.txt"), "mini").unwrap();
    std::fs::write(laptop.work().join("who.txt"), "laptop").unwrap();
    std::fs::write(laptop.work().join("only-here.txt"), "x").unwrap();
    let local = start(|_| {}).await;
    std::fs::write(local.work().join("who.txt"), "local").unwrap();
    local.mount("mini", &mini);
    local.mount("laptop", &laptop);
    let who = |path: String| {
        let local = &local;
        async move {
            let v: Value = get(local, &path)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["content"].as_str().unwrap_or_default().to_string()
        }
    };
    assert_eq!(
        who("/machines/mini/api/contents/who.txt".into()).await,
        "mini"
    );
    assert_eq!(
        who("/machines/laptop/api/contents/who.txt".into()).await,
        "laptop"
    );
    assert_eq!(who("/api/contents/who.txt".into()).await, "local");
    assert_eq!(
        get(&local, "/machines/mini/api/contents/only-here.txt")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // Removing one leaves the other.
    local.machines.remove("mini");
    assert_eq!(
        who("/machines/laptop/api/contents/who.txt".into()).await,
        "laptop"
    );
}

/// Reads frames until the socket ends, within a few seconds: whether a
/// close came, and its code.
async fn until_closed<S>(ws: &mut S) -> Option<u16>
where
    S: futures::Stream<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(WsMsg::Close(frame)))) => return frame.map(|f| u16::from(f.code)),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => panic!("the socket stayed open"),
        }
    }
}

async fn ws_to(
    server: &Server,
    path: &str,
    protocol: Option<&str>,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) {
    let mut req = format!("ws://{}{path}", server.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "Authorization",
        format!("token {}", server.token).parse().unwrap(),
    );
    if let Some(p) = protocol {
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", p.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_machine_closes_its_websockets() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let (mut a, _) = ws_to(&local, "/machines/mini/api/events/subscribe", None).await;
    let (mut b, _) = ws_to(&local, "/machines/mini/api/events/subscribe", None).await;
    // A socket on another machine is left alone.
    let other = machine().await;
    local.mount("other", &other);
    let (mut c, _) = ws_to(&local, "/machines/other/api/events/subscribe", None).await;
    assert!(local.machines.remove("mini"));
    assert_eq!(until_closed(&mut a).await, Some(1001));
    assert_eq!(until_closed(&mut b).await, Some(1001));
    assert!(tokio::time::timeout(Duration::from_millis(300), c.next())
        .await
        .is_err());
    // Replacing a machine closes its sockets as well.
    local.mount("other", &other);
    assert_eq!(until_closed(&mut c).await, Some(1001));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_ends_when_either_side_closes() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let resp = request(&local, reqwest::Method::POST, "/machines/mini/api/kernels")
        .json(&json!({"name": "fake"}))
        .send()
        .await
        .unwrap();
    let kernel: Value = resp.json().await.unwrap();
    let id = kernel["id"].as_str().unwrap().to_string();
    let connections = || {
        let (mini, id) = (&mini, id.clone());
        async move {
            let v: Value = get(mini, &format!("/api/kernels/{id}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["connections"].as_u64().unwrap()
        }
    };
    let path = format!("/machines/mini/api/kernels/{id}/channels?session_id=a");
    let (mut ws, _) = ws_to(&local, &path, None).await;
    assert_eq!(connections().await, 1);
    // The page closes: the machine's socket goes too.
    ws.send(WsMsg::Close(None)).await.unwrap();
    until_closed(&mut ws).await;
    let mut left = 1;
    for _ in 0..50 {
        left = connections().await;
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(left, 0, "the machine's socket stayed open");
    // The machine closes (a room that doesn't exist): the page's socket
    // ends with the machine's code and reason.
    let (mut ws, _) = ws_to(
        &local,
        "/machines/mini/api/collaboration/room/json:notebook:nofile",
        None,
    )
    .await;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
        {
            Some(Ok(WsMsg::Close(Some(frame)))) => {
                assert_eq!(u16::from(frame.code), 4404);
                assert!(frame.reason.contains("Unknown file id"), "{frame:?}");
                break;
            }
            Some(Ok(_)) => continue,
            other => panic!("{other:?}"),
        }
    }
}

// ----- entry points ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn contents_http_round_trip() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let url = |p: &str| format!("/machines/mini/api/contents{p}");
    // PUT
    let resp = request(&local, reqwest::Method::PUT, &url("/a.txt"))
        .json(&json!({"type": "file", "format": "text", "content": "hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_eq!(
        std::fs::read_to_string(mini.work().join("a.txt")).unwrap(),
        "hello"
    );
    assert!(!local.work().join("a.txt").exists());
    // GET, a directory listing, and a path with an escaped space.
    let v: Value = get(&local, &url("/a.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (v["content"].as_str(), v["path"].as_str()),
        (Some("hello"), Some("a.txt"))
    );
    let v: Value = get(&local, &url(""))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["content"][0]["name"], "a.txt");
    let resp = request(&local, reqwest::Method::PUT, &url("/b%20c.txt"))
        .json(&json!({"type": "file", "format": "text", "content": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert!(mini.work().join("b c.txt").exists());
    // POST (a new notebook), PATCH (rename), DELETE.
    let resp = request(&local, reqwest::Method::POST, &url(""))
        .json(&json!({"type": "notebook"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let made: Value = resp.json().await.unwrap();
    let name = made["name"].as_str().unwrap().to_string();
    assert!(mini.work().join(&name).exists());
    let resp = request(&local, reqwest::Method::PATCH, &url(&format!("/{name}")))
        .json(&json!({"path": "renamed.ipynb"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(mini.work().join("renamed.ipynb").exists());
    let resp = request(&local, reqwest::Method::DELETE, &url("/renamed.ipynb"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(!mini.work().join("renamed.ipynb").exists());
    // A status the machine gives passes through.
    assert_eq!(
        get(&local, &url("/missing.txt"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // `files` streams a file, and HEAD and Range work through it.
    let resp = get(&local, "/machines/mini/files/a.txt")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-security-policy"],
        "sandbox allow-scripts; frame-ancestors 'self'"
    );
    assert_eq!(resp.text().await.unwrap(), "hello");
    let resp = get(&local, "/machines/mini/files/a.txt")
        .header("Range", "bytes=1-2")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.text().await.unwrap(), "el");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rooms_websocket() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let nb = json!({"cells": [], "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    let resp = request(
        &local,
        reqwest::Method::PUT,
        "/machines/mini/api/contents/room.ipynb",
    )
    .json(&json!({"type": "notebook", "format": "json", "content": nb}))
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 201);
    let resp = request(
        &local,
        reqwest::Method::PUT,
        "/machines/mini/api/collaboration/session/room.ipynb",
    )
    .json(&json!({"format": "json", "type": "notebook"}))
    .send()
    .await
    .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let session: Value = resp.json().await.unwrap();
    let room = format!("json:notebook:{}", session["fileId"].as_str().unwrap());
    let (mut ws, _) = ws_to(
        &local,
        &format!(
            "/machines/mini/api/collaboration/room/{room}?sessionId={}",
            session["sessionId"].as_str().unwrap()
        ),
        None,
    )
    .await;
    // A y-sync step 1 with an empty state vector, which the room answers
    // with the document's state.
    ws.send(WsMsg::Binary(vec![0, 0, 1, 0].into()))
        .await
        .unwrap();
    let mut got = false;
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
        if let WsMsg::Binary(b) = frame {
            assert!(!b.is_empty());
            got = true;
            break;
        }
    }
    assert!(got, "the room sent nothing through the mount");
    // The room is the machine's, not the local server's.
    assert!(local.work().read_dir().unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn kernel_channels_with_the_subprotocol() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let spec: Value = get(&local, "/machines/mini/api/kernelspecs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        spec["kernelspecs"]["fake"]["spec"]["display_name"],
        "Machine fake"
    );
    let resp = request(&local, reqwest::Method::POST, "/machines/mini/api/kernels")
        .json(&json!({"name": "fake"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let kernel: Value = resp.json().await.unwrap();
    let id = kernel["id"].as_str().unwrap();
    let on_mini: Value = get(&mini, "/api/kernels")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(on_mini.as_array().unwrap().len(), 1);
    let on_local: Value = get(&local, "/api/kernels")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(on_local, json!([]));

    let (mut ws, resp) = ws_to(
        &local,
        &format!("/machines/mini/api/kernels/{id}/channels?session_id=abc"),
        Some("v1.kernel.websocket.jupyter.org"),
    )
    .await;
    assert_eq!(
        resp.headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok()),
        Some("v1.kernel.websocket.jupyter.org")
    );
    let exec = KernelMessage::new(
        "execute_request",
        "sess",
        "me",
        &ExecuteRequest::new("ping"),
    );
    ws.send(WsMsg::Binary(
        v1::encode(Channel::Shell, &exec).unwrap().into(),
    ))
    .await
    .unwrap();
    let (mut reply, mut idle, mut text) = (false, false, String::new());
    while !(reply && idle) {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message in time")
            .unwrap()
            .unwrap();
        let WsMsg::Binary(b) = frame else { continue };
        let (channel, msg) = v1::decode(&b).unwrap();
        if msg.parent_id() != Some(exec.msg_id()) {
            continue;
        }
        match (channel, msg.msg_type()) {
            (Channel::Shell, "execute_reply") => reply = true,
            (Channel::IoPub, "status") if msg.content["execution_state"] == "idle" => idle = true,
            (Channel::IoPub, "stream") => text.push_str(msg.content["text"].as_str().unwrap()),
            _ => {}
        }
    }
    assert_eq!(text, "echo: ping\n");
    // Without the subprotocol, the machine speaks its legacy framing, and
    // the mount does not choose one for it.
    let (_ws, resp) = ws_to(
        &local,
        &format!("/machines/mini/api/kernels/{id}/channels?session_id=def"),
        None,
    )
    .await;
    assert!(resp.headers().get("Sec-WebSocket-Protocol").is_none());
    // Shutting the kernel down on the machine closes the page's socket.
    let resp = request(
        &local,
        reqwest::Method::DELETE,
        &format!("/machines/mini/api/kernels/{id}"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(until_closed(&mut ws).await.is_some());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_terminals_websocket() {
    let mini = machine().await;
    let local = start(|_| {}).await;
    local.mount("mini", &mini);
    let resp = request(
        &local,
        reqwest::Method::POST,
        "/machines/mini/api/terminals",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status(), 200);
    let term: Value = resp.json().await.unwrap();
    let name = term["name"].as_str().unwrap();
    let (mut ws, _) = ws_to(
        &local,
        &format!("/machines/mini/terminals/websocket/{name}"),
        None,
    )
    .await;
    let first = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first, WsMsg::Text(json!(["setup", {}]).to_string().into()));
    ws.send(WsMsg::Text(
        json!(["stdin", "echo done-$((20+22))\n"])
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut out = String::new();
    while !out.contains("done-42") {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("output in time")
            .unwrap()
            .unwrap();
        let WsMsg::Text(t) = frame else { continue };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        out.push_str(v[1].as_str().unwrap_or_default());
    }
    // The terminal is on the machine.
    let list: Value = get(&mini, "/api/terminals")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let list: Value = get(&local, "/api/terminals")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list, json!([]));
}
