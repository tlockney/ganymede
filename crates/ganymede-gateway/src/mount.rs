//! Another Ganymede server mounted into this one: a connected machine
//! (ganymede-ui#697). The embedding app adds a machine by id, URL and
//! optional token; the server then forwards an allowlist of routes below
//! `/machines/{id}/` to it, HTTP and websocket ([`route`]), so a page that
//! only talks to its own origin can work on that machine's files, rooms and
//! kernels.
//!
//! This module is the framework-free part: the registry, the path rules,
//! what is stripped from a request and a response, and the two ways of
//! reaching the machine ([`Machine::forward`], [`Machine::connect`]).
//! `ganymede-server`'s `mount` module checks who may use the mount and
//! joins these to its router.
//!
//! The machine's credential lives here and is added to each forwarded
//! request. The page's own credentials, the local token, cookie, `Origin`
//! and so on, never leave: see [`sanitize_request`].

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Sent on every forwarded request, so a machine with `[tailscale] allow`
/// takes it for a program on an allowed device rather than a web page. The
/// page's own value is dropped first.
pub const CLIENT_HEADER: (&str, &str) = ("Ganymede-Client", "ganymede-mount");

/// Longest machine id, and what it is made of: `[A-Za-z0-9_-]`.
const MAX_ID: usize = 64;

// ----- paths --------------------------------------------------------------------

/// What an allowed path is, for the sandbox policy the server sets on
/// its response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `nbconvert/html/...`: a published page, which may download.
    Page,
    /// Everything else.
    Other,
}

/// Why a path is not forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Not a plain path: `..`, an encoded or raw slash or backslash, a NUL,
    /// an empty segment, bad percent-encoding.
    Malformed,
    /// A plain path, but not on the allowlist.
    NotAllowed,
}

/// `s` with its `%XX` escapes decoded, or None when one is malformed or the
/// result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Checks `rest`, the request path after `/machines/{id}/` exactly as it was
/// sent (still percent-encoded), and says what it is when it is allowed.
///
/// Each segment is decoded on its own, so an encoded slash can't pass for
/// part of a name: a segment may not decode to `.`, `..`, or anything with
/// a `/`, `\` or NUL, and only the last may be empty (a trailing slash).
/// What is forwarded is `rest` itself, not the decoded form.
///
/// Allowed: `api/status`, `api/me`, `api/contents`, `files`,
/// `api/collaboration`, `api/fileid`, `api/kernels` (with its channels),
/// `api/sessions`, `api/kernelspecs`, `api/terminals`, `terminals/websocket`
/// (the terminal's socket), `api/notebooks/run`,
/// `api/metrics/v1/kernel_usage`, `api/events/subscribe`, `api/nbconvert`
/// and `nbconvert`; subpaths of those that take them. Nothing else, `mcp`
/// and `api/host` included.
pub fn route(rest: &str) -> Result<Kind, Refused> {
    let mut segments = Vec::new();
    let raw: Vec<&str> = rest.split('/').collect();
    for (i, seg) in raw.iter().enumerate() {
        let last = i + 1 == raw.len();
        if seg.is_empty() {
            if last && i > 0 {
                continue;
            }
            return Err(Refused::Malformed);
        }
        let seg = percent_decode(seg).ok_or(Refused::Malformed)?;
        if matches!(seg.as_str(), "." | "..") || seg.contains(['/', '\\', '\0']) {
            return Err(Refused::Malformed);
        }
        segments.push(seg);
    }
    let s: Vec<&str> = segments.iter().map(String::as_str).collect();
    // `exact`: the path is the route; otherwise anything below it too.
    let kind = match s.as_slice() {
        ["api", "status"] | ["api", "me"] | ["api", "notebooks", "run"] => Kind::Other,
        ["api", "events", "subscribe"] | ["api", "nbconvert"] => Kind::Other,
        ["nbconvert", "html", ..] => Kind::Page,
        ["files", ..] | ["nbconvert", ..] => Kind::Other,
        ["api", "contents" | "collaboration" | "fileid" | "kernels", ..]
        | ["api", "sessions" | "kernelspecs" | "terminals", ..]
        | ["api", "metrics", "v1", "kernel_usage", ..]
        | ["terminals", "websocket", _] => Kind::Other,
        _ => return Err(Refused::NotAllowed),
    };
    Ok(kind)
}

// ----- what is stripped ---------------------------------------------------------

/// Headers that are never forwarded to the machine: the page's credentials
/// and origin, hop-by-hop headers, what a browser adds that would make the
/// machine take the request for a web page's (`Sec-Fetch-*`), and proxy and
/// tailnet identity headers a page could forge.
fn strip_request_header(name: &str) -> bool {
    matches!(
        name,
        "cookie"
            | "authorization"
            | "origin"
            | "referer"
            | "x-xsrftoken"
            | "x-csrftoken"
            | "ganymede-client"
            | "host"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "expect"
            | "forwarded"
            | "x-real-ip"
            | "x-scheme"
    ) || name.starts_with("sec-fetch-")
        || name.starts_with("sec-websocket-")
        || name.starts_with("x-forwarded-")
        || name.starts_with("tailscale-")
}

/// `headers` as the machine should get them: without the page's credentials
/// and the rest of `strip_request_header`, and without any header the
/// `Connection` header names.
pub fn sanitize_request(headers: &HeaderMap) -> HeaderMap {
    let named: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .collect();
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if !strip_request_header(name.as_str()) && !named.iter().any(|n| n == name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// `query` without the `token` and `_xsrf` parameters (the local server's
/// credentials), the other pairs kept as they were sent. None when nothing
/// is left.
pub fn sanitize_query(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query?
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            let key = percent_decode(&key.replace('+', " "));
            !matches!(key.as_deref(), Some("token" | "_xsrf"))
        })
        .filter(|pair| !pair.is_empty())
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

/// The response headers of the machine's that are passed on, and no
/// others: what describes the body and its caching. Cookies, policies
/// (`Content-Security-Policy`, CORS, HSTS) and the rest are the mount's
/// to set, never the machine's.
fn pass_response_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-length"
            | "content-disposition"
            | "content-encoding"
            | "content-language"
            | "content-range"
            | "accept-ranges"
            | "cache-control"
            | "etag"
            | "last-modified"
            | "expires"
            | "vary"
            | "x-content-type-options"
    )
}

/// The machine's response headers as the page gets them: only those
/// `pass_response_header` lists. A `Location` that is a path on the
/// machine gets the mount's prefix (`mount_prefix`, such as
/// `/machines/mini`) so the page stays under it, but only when the path
/// (before any `?` or `#`) is one [`route`] allows. Anything else is
/// dropped: a whole URL would send the page to the machine itself, and
/// `/../..` in any encoding would climb out of the mount onto this
/// server, where a followed 307 repeats the method, body and credentials.
pub fn sanitize_response(headers: &HeaderMap, base_path: &str, mount_prefix: &str) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if name == reqwest::header::LOCATION {
            let Some(loc) = value.to_str().ok().filter(|l| l.starts_with('/')) else {
                continue;
            };
            let base = base_path.trim_end_matches('/');
            let tail = if base.is_empty() {
                Some(loc)
            } else {
                loc.strip_prefix(base)
                    .filter(|t| t.is_empty() || t.starts_with(['/', '?', '#']))
            };
            let Some(tail) = tail else { continue };
            // The page follows it below the mount, so it must be a path
            // the mount would forward itself: not one that climbs out
            // (`/../..`, in any encoding) onto this server.
            let path = tail.split(['?', '#']).next().unwrap_or("");
            if route(path.strip_prefix('/').unwrap_or("")).is_err() || !path.starts_with('/') {
                continue;
            }
            if let Ok(v) = HeaderValue::from_str(&format!("{mount_prefix}{tail}")) {
                out.append(name.clone(), v);
            }
        } else if pass_response_header(name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

// ----- the registry ---------------------------------------------------------------

/// The machines mounted on a server, changeable while it runs. Nothing is
/// mounted until [`Machines::add`].
#[derive(Default)]
pub struct Machines {
    map: RwLock<HashMap<String, Arc<Machine>>>,
    /// Given to machines added from now on; `None`: [`RESPONSE_TIMEOUT`].
    response_timeout: Option<Duration>,
}

/// How long a machine has to start answering a forwarded request.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

impl Machines {
    pub fn new() -> Machines {
        Machines::default()
    }

    /// [`Machines::new`], with machines given `timeout` instead of
    /// [`RESPONSE_TIMEOUT`] to start answering.
    pub fn with_response_timeout(timeout: Duration) -> Machines {
        Machines {
            response_timeout: Some(timeout),
            ..Machines::default()
        }
    }

    /// Mounts the server at `url` (`http(s)://host[:port][/base]`) as
    /// `id`, authenticating with `token` when given (otherwise the
    /// machine may let it in by tailnet identity). An `id` already
    /// mounted is replaced, and the replaced machine's open websockets
    /// are closed. No request is made yet.
    pub fn add(&self, id: &str, url: &str, token: Option<&str>) -> Result<(), String> {
        let mut machine = Machine::new(id, url, token)?;
        if let Some(t) = self.response_timeout {
            machine.response_timeout = t;
        }
        let machine = Arc::new(machine);
        let old = self
            .map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), machine);
        if let Some(old) = old {
            old.close();
        }
        Ok(())
    }

    /// Unmounts `id`, closing its open websockets. Whether it was mounted.
    pub fn remove(&self, id: &str) -> bool {
        let old = self
            .map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        old.map(|m| m.close()).is_some()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Machine>> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// The ids mounted now, sorted.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
}

/// One mounted machine.
pub struct Machine {
    id: String,
    /// Ends in `/`, so a path joins below any base path.
    base: reqwest::Url,
    token: Option<String>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    /// True once removed or replaced.
    closed: watch::Sender<bool>,
    /// How long the machine has to start its answer.
    response_timeout: Duration,
}

impl Machine {
    fn new(id: &str, url: &str, token: Option<&str>) -> Result<Machine, String> {
        if id.is_empty()
            || id.len() > MAX_ID
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(format!(
                "machine id {id:?} must be 1 to {MAX_ID} characters of A-Z, a-z, 0-9, _ and -"
            ));
        }
        let mut base = reqwest::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(format!("{url}: not an http or https URL"));
        }
        if base.query().is_some()
            || base.fragment().is_some()
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(format!(
                "{url}: give the URL without a query, fragment or credentials"
            ));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let tls = Arc::new(super::tls_config().map_err(|e| e.to_string())?);
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured((*tls).clone())
            .connect_timeout(Duration::from_secs(10))
            // A redirect is the page's to follow, below the mount.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Machine {
            id: id.to_string(),
            base,
            token: token.filter(|t| !t.is_empty()).map(String::from),
            http,
            tls,
            closed: watch::channel(false).0,
            response_timeout: RESPONSE_TIMEOUT,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn close(&self) {
        let _ = self.closed.send(true);
    }

    fn url(&self, rest: &str, query: Option<&str>) -> Result<reqwest::Url, String> {
        let mut url = self.base.join(rest).map_err(|e| e.to_string())?;
        // `rest` was checked by `route`; it can only stay below the base.
        if !url.path().starts_with(self.base.path()) {
            return Err("path leaves the machine's base path".into());
        }
        url.set_query(sanitize_query(query).as_deref());
        Ok(url)
    }

    /// The credentials this mount sends, and nothing of the page's.
    fn credentials(&self, headers: &mut HeaderMap) -> Result<(), String> {
        headers.insert(
            HeaderName::from_static("ganymede-client"),
            HeaderValue::from_static(CLIENT_HEADER.1),
        );
        if let Some(t) = &self.token {
            let v = HeaderValue::from_str(&format!("token {t}")).map_err(|e| e.to_string())?;
            headers.insert(reqwest::header::AUTHORIZATION, v);
        }
        Ok(())
    }

    /// Sends the page's request to the machine: `rest` is a path that
    /// [`route`] allowed. The page's credentials are not sent
    /// ([`sanitize_request`], [`sanitize_query`]); this machine's are.
    /// `mount_prefix` is where the machine is mounted, for a redirect.
    /// Fails, with a message for the page, when the machine can't be
    /// reached.
    pub async fn forward(
        &self,
        method: Method,
        rest: &str,
        query: Option<&str>,
        headers: &HeaderMap,
        body: Bytes,
        mount_prefix: &str,
    ) -> Result<Forwarded, ForwardError> {
        let url = self.url(rest, query).map_err(ForwardError::Unreachable)?;
        let mut out = sanitize_request(headers);
        self.credentials(&mut out)
            .map_err(ForwardError::Unreachable)?;
        let mut req = self.http.request(method, url).headers(out);
        if !body.is_empty() {
            req = req.body(body);
        }
        let resp = tokio::time::timeout(self.response_timeout, req.send())
            .await
            .map_err(|_| {
                ForwardError::TimedOut(format!(
                    "machine {} did not answer within {} seconds",
                    self.id,
                    self.response_timeout.as_secs().max(1)
                ))
            })?
            .map_err(|e| {
                ForwardError::Unreachable(format!(
                    "machine {} is unreachable: {}",
                    self.id,
                    describe(e)
                ))
            })?;
        Ok(Forwarded {
            status: resp.status(),
            headers: sanitize_response(resp.headers(), self.base.path(), mount_prefix),
            body: resp,
        })
    }

    /// Opens the machine's websocket at `rest`, offering the page's
    /// `protocols`, with this machine's credentials and no `Origin`.
    pub async fn connect(
        &self,
        rest: &str,
        query: Option<&str>,
        protocols: &[String],
    ) -> Result<Upstream, String> {
        let mut url = self.url(rest, query)?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| "cannot make a websocket URL".to_string())?;
        let mut req = url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
        let mut creds = HeaderMap::new();
        self.credentials(&mut creds)?;
        for (name, value) in &creds {
            req.headers_mut().insert(name.clone(), value.clone());
        }
        if !protocols.is_empty() {
            let v = HeaderValue::from_str(&protocols.join(", ")).map_err(|e| e.to_string())?;
            req.headers_mut().insert("Sec-WebSocket-Protocol", v);
        }
        let connector = tokio_tungstenite::Connector::Rustls(self.tls.clone());
        let attempt =
            tokio_tungstenite::connect_async_tls_with_config(req, None, true, Some(connector));
        let (ws, resp) = tokio::time::timeout(Duration::from_secs(10), attempt)
            .await
            .map_err(|_| format!("machine {} is unreachable: timed out", self.id))?
            .map_err(|e| format!("machine {} is unreachable: {e}", self.id))?;
        let protocol = resp
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        Ok(Upstream {
            ws,
            protocol,
            closed: self.closed.subscribe(),
        })
    }
}

/// A transport failure in words, without the URL (the page has it).
fn describe(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        e.without_url().to_string()
    }
}

/// Why a request did not get an answer from the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardError {
    /// Not reached, or the request could not be made: a 502.
    Unreachable(String),
    /// Reached, but silent too long: a 504.
    TimedOut(String),
}

/// The machine's answer: its status, the headers the page may have, and
/// a body to stream.
pub struct Forwarded {
    pub status: reqwest::StatusCode,
    pub headers: HeaderMap,
    body: reqwest::Response,
}

impl Forwarded {
    /// The body as it arrives. An error ends it.
    pub fn into_stream(self) -> impl futures::Stream<Item = io::Result<Bytes>> + Send + 'static {
        futures::stream::unfold(Some(self.body), |resp| async move {
            let mut resp = resp?;
            match resp.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), Some(resp))),
                Ok(None) => None,
                Err(e) => Some((Err(io::Error::other(e.without_url().to_string())), None)),
            }
        })
    }
}

// ----- websockets -------------------------------------------------------------------

/// A websocket frame, as the page's side of the proxy needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    /// With the code and reason when the sender gave them.
    Close(Option<(u16, String)>),
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// An open websocket to a machine.
pub struct Upstream {
    ws: Socket,
    protocol: Option<String>,
    closed: watch::Receiver<bool>,
}

/// The sending half of an [`Upstream`].
pub struct UpstreamTx(futures::stream::SplitSink<Socket, WsMessage>);

/// The receiving half of an [`Upstream`].
pub struct UpstreamRx(futures::stream::SplitStream<Socket>);

impl Upstream {
    /// The subprotocol the machine chose, if any.
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// The two halves, so each direction can be driven on its own, and
    /// the handle to wait for the machine's removal with.
    pub fn split(self) -> (UpstreamTx, UpstreamRx, Removal) {
        let (tx, rx) = self.ws.split();
        (UpstreamTx(tx), UpstreamRx(rx), Removal(self.closed))
    }
}

impl UpstreamRx {
    /// The next frame from the machine; `None` when it ended the socket.
    /// Pings and pongs are answered by the socket itself and skipped.
    pub async fn recv(&mut self) -> Option<Frame> {
        loop {
            let frame = match self.0.next().await? {
                Ok(f) => f,
                Err(_) => return None,
            };
            match frame {
                WsMessage::Text(t) => return Some(Frame::Text(t.as_str().to_string())),
                WsMessage::Binary(b) => return Some(Frame::Binary(b.to_vec())),
                WsMessage::Close(c) => {
                    return Some(Frame::Close(
                        c.map(|c| (u16::from(c.code), c.reason.as_str().to_string())),
                    ))
                }
                _ => {}
            }
        }
    }
}

impl UpstreamTx {
    /// Sends `frame` to the machine; an error means the socket is gone.
    pub async fn send(&mut self, frame: Frame) -> Result<(), String> {
        let msg = match frame {
            Frame::Text(t) => WsMessage::Text(t.into()),
            Frame::Binary(b) => WsMessage::Binary(b.into()),
            Frame::Close(c) => WsMessage::Close(c.and_then(|(code, reason)| {
                let code = CloseCode::from(code);
                code.is_allowed().then(|| CloseFrame {
                    code,
                    reason: reason.into(),
                })
            })),
        };
        self.0.send(msg).await.map_err(|e| e.to_string())
    }

    /// Closes the socket, giving up on a machine that does not answer.
    pub async fn close(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(2), self.0.close()).await;
    }
}

/// Completes when the machine is removed or replaced.
pub struct Removal(watch::Receiver<bool>);

impl Removal {
    pub async fn wait(&mut self) {
        let _ = self.0.wait_for(|c| *c).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn the_allowlist_is_exact() {
        for ok in [
            "api/status",
            "api/me",
            "api/contents",
            "api/contents/",
            "api/contents/a/b%20c.ipynb",
            "files/x/y.html",
            "api/collaboration/session/a.ipynb",
            "api/collaboration/room/json:notebook:abc",
            "api/fileid/index",
            "api/kernels",
            "api/kernels/k1/channels",
            "api/sessions/s",
            "api/kernelspecs",
            "api/terminals/1",
            "terminals/websocket/1",
            "api/notebooks/run",
            "api/metrics/v1/kernel_usage/get_usage/k1",
            "api/events/subscribe",
            "api/nbconvert",
            "nbconvert/html/a.ipynb",
        ] {
            assert!(route(ok).is_ok(), "{ok}");
        }
        for no in [
            "mcp",
            "mcp/",
            "api/host/grants/a.ipynb",
            "api",
            "api/",
            "api/config/x",
            "api/statusx",
            "api/status/x",
            "api/me/x",
            "api/notebooks/run/x",
            "api/notebooks",
            "api/metrics/v1",
            "api/events/subscribe/x",
            "api/kernelsx",
            "terminals/websocket",
            "terminals/websocket/1/2",
            "lab",
            "lsp/status",
            "login",
            "File",
            "Files/x",
            "API/status",
            "kernelspecs/a/logo.png",
        ] {
            assert_eq!(route(no), Err(Refused::NotAllowed), "{no}");
        }
        assert_eq!(route("files/a"), Ok(Kind::Other));
        assert_eq!(route("nbconvert/html/a"), Ok(Kind::Page));
        assert_eq!(route("nbconvert/%68tml/a"), Ok(Kind::Page));
        assert_eq!(route("nbconvert/script/a"), Ok(Kind::Other));
        assert_eq!(route("api/contents/a"), Ok(Kind::Other));
    }

    #[test]
    fn traversal_and_encoded_separators_are_refused() {
        for bad in [
            "api/contents/../../mcp",
            "api/contents/..",
            "api/contents/./a",
            "api/contents/%2e%2e/%2e%2e/mcp",
            "api/contents/%2E%2E",
            "api/contents/.%2e/x",
            "api/contents/a%2fb",
            "api/contents/a%2Fb",
            "api/contents/a%5cb",
            "api/contents/a%5Cb",
            "api/contents/a\\b",
            "api/contents/a%00b",
            "api/contents//a",
            "api//contents",
            "/api/status",
            "api%2fstatus",
            "api/%2e%2e/api/host/grants/x",
            "api/contents/%zz",
            "api/contents/%2",
            "api/contents/%ff",
            "files/..%2f..%2fetc/passwd",
            "",
        ] {
            assert_eq!(route(bad), Err(Refused::Malformed), "{bad:?}");
        }
        // An allowed prefix doesn't excuse what follows, and a double
        // encoding is a literal name, not a separator.
        assert!(route("api/contents/a%252fb").is_ok());
    }

    #[test]
    fn the_pages_credentials_are_stripped() {
        let h = headers(&[
            ("cookie", "username-x=1; _xsrf=2"),
            ("authorization", "token local"),
            ("origin", "http://127.0.0.1:1"),
            ("referer", "http://127.0.0.1:1/lab"),
            ("x-xsrftoken", "x"),
            ("x-csrftoken", "x"),
            ("ganymede-client", "page"),
            ("host", "127.0.0.1:1"),
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("upgrade", "websocket"),
            ("transfer-encoding", "chunked"),
            ("sec-fetch-site", "same-origin"),
            ("sec-fetch-mode", "cors"),
            ("sec-websocket-key", "k"),
            ("x-forwarded-for", "1.2.3.4"),
            ("x-forwarded-proto", "https"),
            ("forwarded", "for=1.2.3.4"),
            ("tailscale-user-login", "a@b"),
            ("accept", "application/json"),
            ("content-type", "application/json"),
            ("if-match", "\"e\""),
            ("range", "bytes=0-1"),
        ]);
        let out = sanitize_request(&h);
        let mut names: Vec<&str> = out.keys().map(|k| k.as_str()).collect();
        names.sort();
        assert_eq!(names, ["accept", "content-type", "if-match", "range"]);
    }

    #[test]
    fn local_query_credentials_are_stripped() {
        let s = |q| sanitize_query(Some(q));
        assert_eq!(s("token=t"), None);
        assert_eq!(s("token=t&_xsrf=x"), None);
        assert_eq!(s("a=1&token=t&b=%20c"), Some("a=1&b=%20c".into()));
        assert_eq!(s("%74oken=t&a=1"), Some("a=1".into()));
        assert_eq!(s("a=1&_xsrf=x&_xsrf=y&token"), Some("a=1".into()));
        assert_eq!(s("tokens=1&xtoken=2"), Some("tokens=1&xtoken=2".into()));
        assert_eq!(
            s("path=a%2Fb&content=0"),
            Some("path=a%2Fb&content=0".into())
        );
        assert_eq!(sanitize_query(None), None);
    }

    #[test]
    fn responses_lose_cookies_and_keep_the_rest() {
        let h = headers(&[
            ("set-cookie", "a=b"),
            ("set-cookie", "c=d"),
            ("strict-transport-security", "max-age=1"),
            ("transfer-encoding", "chunked"),
            ("content-type", "text/html"),
            ("etag", "\"x\""),
            ("x-content-type-options", "nosniff"),
            ("content-security-policy", "default-src *"),
            ("access-control-allow-origin", "*"),
            ("x-anything-else", "1"),
        ]);
        let out = sanitize_response(&h, "/", "/machines/m");
        assert!(out.get("set-cookie").is_none());
        assert!(out.get("strict-transport-security").is_none());
        assert!(out.get("transfer-encoding").is_none());
        assert!(out.get("access-control-allow-origin").is_none());
        assert!(out.get("x-anything-else").is_none());
        assert_eq!(out["content-type"], "text/html");
        assert_eq!(out["etag"], "\"x\"");
        assert_eq!(out["x-content-type-options"], "nosniff");
        // The machine's policy is not passed on: the server sets its own.
        assert!(out.get("content-security-policy").is_none());
    }

    #[test]
    fn redirects_stay_below_the_mount() {
        let loc = |value: &str, base: &str| {
            let h = headers(&[("location", value)]);
            sanitize_response(&h, base, "/machines/m")
                .get("location")
                .map(|v| v.to_str().unwrap().to_string())
        };
        assert_eq!(
            loc("/api/contents/a", "/"),
            Some("/machines/m/api/contents/a".into())
        );
        assert_eq!(
            loc("/b/files/a?x=1", "/b/"),
            Some("/machines/m/files/a?x=1".into())
        );
        assert_eq!(loc("/other", "/b/"), None);
        assert_eq!(loc("/bad", "/b/"), None);
        assert_eq!(loc("//evil.example/x", "/"), None);
        assert_eq!(loc("http://machine:8888/x", "/"), None);
        assert_eq!(loc("x/y", "/"), None);
        // Nothing that climbs out of the mount, however spelled, and nothing
        // off the allowlist, with or without a base path.
        for base in ["/", "/b/"] {
            let at = |p: &str| format!("{}{p}", base.trim_end_matches('/'));
            for bad in [
                "/../../api/contents/x",
                "/api/contents/../../../api/contents/x",
                "/%2e%2e/%2e%2e/api/host/grants/x",
                "/%2E%2E/api/contents/x",
                "/.%2e/api/contents/x",
                "/api/contents/%2e%2e/%2e%2e/x",
                "/api/contents/a%2fb",
                "/api/contents/a%5cb",
                "/api\\contents/x",
                "/api/contents//x",
                "/mcp",
                "/api/host/grants/x",
                "/lab",
                "/",
                "/?x=/api/status",
                "/#/api/status",
            ] {
                assert_eq!(loc(&at(bad), base), None, "{base} {bad}");
            }
        }
        assert_eq!(
            loc("/api/contents/a%20b?x=/../y#f", "/"),
            Some("/machines/m/api/contents/a%20b?x=/../y#f".into())
        );
    }

    #[test]
    fn machines_are_validated() {
        let m = Machines::new();
        assert!(m.add("mini", "http://127.0.0.1:8888/", Some("t")).is_ok());
        assert!(m.add("a_b-9", "https://box.example/ganymede", None).is_ok());
        for id in ["", "a b", "a/b", "a.b", "é", &"x".repeat(65)] {
            assert!(m.add(id, "http://h/", None).is_err(), "{id:?}");
        }
        for url in [
            "",
            "ftp://h/",
            "file:///x",
            "http://",
            "http://h/?token=x",
            "http://u:p@h/",
            "http://h/#f",
            "not a url",
        ] {
            assert!(m.add("ok", url, None).is_err(), "{url:?}");
        }
        assert_eq!(m.ids(), ["a_b-9", "mini"]);
        assert!(m.remove("mini"));
        assert!(!m.remove("mini"));
        assert!(m.get("mini").is_none());
        assert_eq!(m.get("a_b-9").unwrap().base.path(), "/ganymede/");
    }

    #[test]
    fn urls_stay_below_the_base() {
        let m = Machine::new("m", "http://h:1/base", None).unwrap();
        let u = m
            .url("api/contents/a%20b", Some("token=t&content=0"))
            .unwrap();
        assert_eq!(u.as_str(), "http://h:1/base/api/contents/a%20b?content=0");
        let u = m.url("api/status", Some("token=t")).unwrap();
        assert_eq!(u.as_str(), "http://h:1/base/api/status");
    }
}
