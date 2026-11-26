//! Kernels on another Jupyter server, as `jupyter_server`'s gateway client
//! (`GatewayClient`, `--gateway-url`) has them. The local server keeps
//! contents and its own API; kernels live on the remote one.
//!
//! [`Gateway`] is all three kernel seams at once:
//!
//! - [`KernelspecSource`]: the remote `GET /api/kernelspecs`, cached and
//!   refreshed by [`Gateway::refresh`], since the trait is synchronous.
//! - [`Provisioner`]: `POST /api/kernels` starts a kernel; its process
//!   handle interrupts with `POST .../interrupt` and shuts down with
//!   `DELETE /api/kernels/{id}`. A restart is a new remote kernel.
//! - [`TransportFactory`]: `/api/kernels/{id}/channels` over a websocket,
//!   speaking `v1.kernel.websocket.jupyter.org` when the remote offers it
//!   and the legacy JSON framing otherwise.

pub mod mount;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use ganymede_kernel_host::{
    Error, InterruptMode, KernelProcess, KernelTransport, Kernelspec, KernelspecSource,
    LaunchOptions, LaunchedKernel, Provisioner, Result, TransportFactory, TransportSink,
};
use ganymede_protocol::ws::{legacy, v1};
use ganymede_protocol::{Channel, Message};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const V1_PROTOCOL: &str = "v1.kernel.websocket.jupyter.org";

/// Sent on every request, so a Ganymede server that lets this machine in
/// by tailnet identity (`[tailscale] allow`) accepts it without a token:
/// it marks a program's request, which no web page can send.
const CLIENT_HEADER: (&str, &str) = ("Ganymede-Client", "ganymede-gateway");

/// How soon a refresh retries after one failed, so a gateway that was
/// asleep or off the network shows up again without waiting a minute.
const RETRY: Duration = Duration::from_secs(15);

/// Errors from the remote server, in kernel-host's error type.
fn remote(what: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(format!("gateway: {what}")))
}

/// An error and its causes, outermost first: reqwest's own message leaves
/// out why a request failed (refused, DNS, timeout, TLS), which its
/// sources say.
fn causes(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let more = cause.to_string();
        if !text.contains(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        source = cause.source();
    }
    text
}

/// How much of a remote's error body an error keeps: its first line, cut
/// to [`BRIEF`] characters. A proxy or captive portal may answer with a
/// whole HTML page, which `/api/kernelspecs` would otherwise carry.
const BRIEF: usize = 200;

fn brief(body: &str) -> String {
    let line = body.trim().lines().next().unwrap_or_default().trim_end();
    match line.char_indices().nth(BRIEF) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// A remote Jupyter server's kernels.
pub struct Gateway {
    inner: Arc<Inner>,
}

struct Inner {
    /// Ends in `/`, so `api/...` joins below any base path.
    base: reqwest::Url,
    token: Option<String>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    specs: RwLock<Vec<Kernelspec>>,
    /// The last [`Gateway::refresh`]'s outcome.
    status: RwLock<Status>,
    /// Remote kernel ids by the address of the process handle in the
    /// `LaunchedKernel` that [`Provisioner::launch`] returned, which is how
    /// [`TransportFactory::connect`] finds the kernel to connect to.
    kernels: Mutex<HashMap<usize, String>>,
}

/// What the last [`Gateway::refresh`] found: whether it reached the server
/// and read its kernelspecs, or why it didn't, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// False until a refresh succeeds, and after one fails.
    pub reachable: bool,
    /// Why the last refresh failed, as it failed; taken out of the URL and
    /// token, since the API reports it. None while reachable.
    pub error: Option<String>,
    /// When the last refresh ran; none before the first.
    pub at: Option<std::time::SystemTime>,
}

fn handle_key(process: &Arc<dyn KernelProcess>) -> usize {
    Arc::as_ptr(process) as *const () as usize
}

impl Gateway {
    /// A gateway to the server at `url` (`http(s)://host[:port][/base]`),
    /// authenticating with `token` when given. Without one, a Ganymede
    /// server on the tailnet may let it in by tailnet identity. No request
    /// is made yet.
    pub fn new(url: &str, token: Option<String>) -> Result<Gateway> {
        let mut base = reqwest::Url::parse(url).map_err(|e| remote(format!("{url}: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(remote(format!("{url}: not an http or https URL")));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let tls = Arc::new(tls_config()?);
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured((*tls).clone())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(remote)?;
        Ok(Gateway {
            inner: Arc::new(Inner {
                base,
                token: token.filter(|t| !t.is_empty()),
                http,
                tls,
                specs: RwLock::new(Vec::new()),
                status: RwLock::new(Status::default()),
                kernels: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Fetches the remote kernelspecs into the cache [`KernelspecSource::list`]
    /// reads. Until the first success the list is empty. The outcome is kept
    /// in [`Gateway::status`], including the error a failure is, so the API
    /// can say the gateway isn't answering.
    pub async fn refresh(&self) -> Result<()> {
        let outcome = self.inner.fetch_specs().await;
        let status = match &outcome {
            Ok(specs) => {
                *self.inner.specs.write().unwrap() = specs.clone();
                Status {
                    reachable: true,
                    error: None,
                    at: Some(std::time::SystemTime::now()),
                }
            }
            Err(e) => Status {
                reachable: false,
                error: Some(self.inner.scrub(&e.to_string())),
                at: Some(std::time::SystemTime::now()),
            },
        };
        *self.inner.status.write().unwrap() = status;
        outcome.map(drop)
    }

    /// The last refresh's outcome. Never refreshed: unreachable, with no
    /// error, and no time.
    pub fn status(&self) -> Status {
        self.inner.status.read().unwrap().clone()
    }

    /// The gateway's host, for naming it in the API: never the URL's path,
    /// query or token.
    pub fn host(&self) -> &str {
        self.inner.base.host_str().unwrap_or_default()
    }

    /// The ids of the kernels running on the remote server, whoever
    /// started them.
    pub async fn remote_kernels(&self) -> Result<Vec<String>> {
        let v = self.inner.get("api/kernels").await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|k| k.get("id").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Calls [`Gateway::refresh`] now and then every `every` while it
    /// answers, and every [`RETRY`] while it doesn't, logging failures, for
    /// as long as the gateway exists. A gateway that comes back is found
    /// within [`RETRY`] rather than `every`.
    pub fn refresh_every(&self, every: Duration) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                let reachable = match (Gateway { inner }).refresh().await {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!("{e}");
                        false
                    }
                };
                tokio::time::sleep(next_refresh(reachable, every)).await;
            }
        });
    }
}

/// How long [`Gateway::refresh_every`] waits after a refresh: `every` while
/// the gateway answers, at most [`RETRY`] while it doesn't.
fn next_refresh(reachable: bool, every: Duration) -> Duration {
    if reachable {
        every
    } else {
        every.min(RETRY)
    }
}

fn tls_config() -> Result<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(remote)?
        .with_platform_verifier()
        .map_err(remote)?
        .with_no_client_auth())
}

impl Inner {
    fn url(&self, path: &str) -> Result<reqwest::Url> {
        self.base.join(path).map_err(remote)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let mut req = self
            .http
            .request(method, self.url(path)?)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("token {t}"));
        }
        Ok(req)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = req.send().await.map_err(|e| remote(causes(&e)))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let url = resp.url().path().to_string();
        let body = resp.text().await.unwrap_or_default();
        let reason = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from))
            .unwrap_or(body);
        Err(remote(format!("{url}: {status} {}", brief(&reason))))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let resp = self.send(self.request(reqwest::Method::GET, path)?).await?;
        resp.json().await.map_err(remote)
    }

    /// The remote kernelspecs, each marked as the gateway's.
    async fn fetch_specs(&self) -> Result<Vec<Kernelspec>> {
        let v: Value = self.get("api/kernelspecs").await?;

        let mut specs = Vec::new();
        if let Some(map) = v.get("kernelspecs").and_then(Value::as_object) {
            for (name, entry) in map {
                let Some(spec) = entry.get("spec") else {
                    continue;
                };
                match serde_json::from_value::<Kernelspec>(spec.clone()) {
                    Ok(mut spec) => {
                        spec.name = name.clone();
                        spec.gateway = true;
                        specs.push(spec);
                    }
                    Err(e) => tracing::warn!("gateway kernelspec {name}: {e}"),
                }
            }
        }
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(specs)
    }

    /// `text` with the gateway's URL and token taken out: the API reports a
    /// failed refresh's error, and neither belongs in it.
    fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        let mut secrets = vec![self.base.as_str().trim_end_matches('/')];
        if self.base.path() != "/" {
            secrets.push(self.base.path().trim_end_matches('/'));
        }
        if let Some(t) = &self.token {
            secrets.push(t);
        }
        for secret in secrets {
            if !secret.is_empty() {
                out = out.replace(secret, "");
            }
        }
        out
    }
}

#[async_trait]
impl Provisioner for Gateway {
    async fn launch(&self, spec: &Kernelspec, _opts: LaunchOptions) -> Result<LaunchedKernel> {
        // The remote server has its own root and environment, so the
        // local cwd and env don't apply there.
        let req = self
            .inner
            .request(reqwest::Method::POST, "api/kernels")?
            .json(&serde_json::json!({ "name": spec.name }));
        let model: Value = self.inner.send(req).await?.json().await.map_err(remote)?;
        let id = model
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| remote("kernel model without an id"))?
            .to_string();
        let (alive_tx, alive) = watch::channel(true);
        let process: Arc<dyn KernelProcess> = Arc::new(RemoteKernel {
            inner: self.inner.clone(),
            id: id.clone(),
            alive_tx,
            alive,
        });
        self.inner
            .kernels
            .lock()
            .unwrap()
            .insert(handle_key(&process), id);
        Ok(LaunchedKernel {
            connection: None,
            connection_file: None,
            process,
            // Interrupts go through the remote server's REST API, which
            // signals or messages the kernel as its kernelspec says.
            interrupt_mode: InterruptMode::Signal,
        })
    }
}

impl KernelspecSource for Gateway {
    fn list(&self) -> Result<Vec<Kernelspec>> {
        Ok(self.inner.specs.read().unwrap().clone())
    }
}

/// A kernel on the remote server, standing in for a process.
struct RemoteKernel {
    inner: Arc<Inner>,
    id: String,
    alive_tx: watch::Sender<bool>,
    alive: watch::Receiver<bool>,
}

impl RemoteKernel {
    fn path(&self, rest: &str) -> String {
        format!("api/kernels/{}{rest}", self.id)
    }
}

#[async_trait]
impl KernelProcess for RemoteKernel {
    fn pid(&self) -> Option<u32> {
        None
    }

    async fn interrupt(&self) -> Result<()> {
        let req = self
            .inner
            .request(reqwest::Method::POST, &self.path("/interrupt"))?;
        self.inner.send(req).await.map(drop)
    }

    async fn kill(&self) -> Result<()> {
        if !*self.alive.borrow() {
            return Ok(());
        }
        let req = self
            .inner
            .request(reqwest::Method::DELETE, &self.path(""))?;
        // Still alive on failure, so dropping the handle tries again.
        self.inner.send(req).await?;
        let _ = self.alive_tx.send(false);
        Ok(())
    }

    async fn wait(&self) -> Option<i32> {
        let mut alive = self.alive.clone();
        let _ = alive.wait_for(|a| !a).await;
        None
    }

    fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }

    /// Shuts the remote kernel down in the background, if a runtime is
    /// there to do it, rather than leave it running on the remote server.
    fn kill_now(&self) {
        if !*self.alive.borrow() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(kernel = %self.id, "no runtime to delete the remote kernel; it is left running");
            return;
        };
        let _ = self.alive_tx.send(false);
        if let Ok(req) = self.inner.request(reqwest::Method::DELETE, &self.path("")) {
            let inner = self.inner.clone();
            handle.spawn(async move {
                let _ = inner.send(req).await;
            });
        }
    }
}

impl Drop for RemoteKernel {
    /// The last handle to a remote kernel is gone: delete it there. A
    /// shutdown sends the kernel only a `shutdown_request` (it has no pid
    /// to wait on or kill), and a crash or a restart drops the handle
    /// without one; a remote `jupyter_server` would restart the kernel
    /// or keep it, as its `GatewayKernelManager` expects a `DELETE`.
    fn drop(&mut self) {
        self.kill_now();
        let mut kernels = self.inner.kernels.lock().unwrap();
        kernels.retain(|_, id| *id != self.id);
    }
}

#[async_trait]
impl TransportFactory for Gateway {
    async fn connect(&self, launched: &LaunchedKernel) -> Result<KernelTransport> {
        let id = self
            .inner
            .kernels
            .lock()
            .unwrap()
            .get(&handle_key(&launched.process))
            .cloned()
            .ok_or(Error::Unsupported(
                "connect: not a kernel this gateway launched",
            ))?;
        let mut url = self.inner.url(&format!("api/kernels/{id}/channels"))?;
        let ws_scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(ws_scheme)
            .map_err(|_| remote("cannot make a websocket URL"))?;
        url.query_pairs_mut()
            .append_pair("session_id", &uuid::Uuid::new_v4().to_string());
        let mut req = url.as_str().into_client_request().map_err(remote)?;
        let headers = req.headers_mut();
        headers.insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(V1_PROTOCOL),
        );
        headers.insert(CLIENT_HEADER.0, HeaderValue::from_static(CLIENT_HEADER.1));
        if let Some(t) = &self.inner.token {
            let value = HeaderValue::from_str(&format!("token {t}")).map_err(remote)?;
            headers.insert("Authorization", value);
        }
        let connector = tokio_tungstenite::Connector::Rustls(self.inner.tls.clone());
        let (ws, resp) =
            tokio_tungstenite::connect_async_tls_with_config(req, None, true, Some(connector))
                .await
                .map_err(remote)?;
        let v1 = resp
            .headers()
            .get("Sec-WebSocket-Protocol")
            .is_some_and(|p| p.as_bytes() == V1_PROTOCOL.as_bytes());
        Ok(websocket_transport(ws, v1))
    }
}

fn encode(v1: bool, channel: Channel, msg: &Message) -> Result<WsMessage> {
    Ok(if v1 {
        WsMessage::Binary(v1::encode(channel, msg)?.into())
    } else {
        match legacy::encode(channel, msg)? {
            legacy::Frame::Text(t) => WsMessage::Text(t.into()),
            legacy::Frame::Binary(b) => WsMessage::Binary(b.into()),
        }
    })
}

fn decode(v1: bool, frame: WsMessage) -> Option<ganymede_protocol::Result<(Channel, Message)>> {
    match frame {
        WsMessage::Binary(b) if v1 => Some(v1::decode(&b)),
        WsMessage::Binary(b) => Some(legacy::decode_binary(&b)),
        WsMessage::Text(t) => Some(legacy::decode_text(&t)),
        _ => None,
    }
}

/// Splits the socket into a writer task fed by the sink and a reader task
/// feeding `incoming`; either ending closes the transport.
fn websocket_transport<S>(ws: tokio_tungstenite::WebSocketStream<S>, v1: bool) -> KernelTransport
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut write, mut read) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<WsMessage>(256);
    let (in_tx, incoming) = mpsc::channel(256);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            let closing = matches!(frame, WsMessage::Close(_));
            if write.send(frame).await.is_err() || closing {
                break;
            }
        }
        let _ = write.close().await;
    });
    tokio::spawn(async move {
        while let Some(frame) = read.next().await {
            let Ok(frame) = frame else { break };
            match decode(v1, frame) {
                Some(Ok(msg)) => {
                    if in_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => tracing::warn!("gateway websocket: {e}"),
                None => {}
            }
        }
        writer.abort();
    });
    KernelTransport {
        sink: Arc::new(WebSocketSink { out: out_tx, v1 }),
        incoming,
    }
}

struct WebSocketSink {
    out: mpsc::Sender<WsMessage>,
    v1: bool,
}

#[async_trait]
impl TransportSink for WebSocketSink {
    async fn send(&self, channel: Channel, msg: Message) -> Result<()> {
        let frame = encode(self.v1, channel, &msg)?;
        self.out
            .send(frame)
            .await
            .map_err(|_| Error::TransportClosed)
    }

    /// The remote server watches the kernel's heartbeat itself; here the
    /// socket being open is the sign of life.
    async fn heartbeat(&self, _timeout: Duration) -> Result<bool> {
        Ok(!self.out.is_closed())
    }

    async fn close(&self) {
        let _ = self.out.send(WsMessage::Close(None)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A stand-in for a gateway that answers 503 until `answering` is set,
    /// then serves one kernelspec: a server asleep and then awake, without
    /// a port race with a real one. Its address.
    async fn stub() -> (String, Arc<AtomicBool>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let answering = Arc::new(AtomicBool::new(false));
        let up = answering.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let up = up.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let (status, body) = if up.load(Ordering::SeqCst) {
                        let body = serde_json::json!({
                            "default": "fake",
                            "kernelspecs": {
                                "fake": {"spec": {
                                    "argv": [],
                                    "display_name": "Remote fake",
                                    "language": "fake",
                                }},
                            },
                        });
                        ("200 OK", body.to_string())
                    } else {
                        let body = serde_json::json!({"message": "asleep"});
                        ("503 Service Unavailable", body.to_string())
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (addr.to_string(), answering)
    }

    #[tokio::test]
    async fn status_says_when_the_gateway_isnt_answering_and_when_it_is() {
        let (addr, answering) = stub().await;
        let gateway = Gateway::new(
            &format!("http://{addr}/secret-base/"),
            Some("s3cret".into()),
        )
        .unwrap();
        assert_eq!(
            gateway.status(),
            Status::default(),
            "nothing before the first refresh"
        );

        gateway.refresh().await.unwrap_err();
        let status = gateway.status();
        assert!(!status.reachable);
        let error = status.error.clone().unwrap();
        assert!(error.contains("asleep"), "{error}");
        // Neither the URL's path nor the token is reported.
        assert!(!error.contains("secret-base"), "{error}");
        assert!(!error.contains("s3cret"), "{error}");
        assert!(status.at.is_some());
        // A failed refresh keeps the last kernelspecs there were.
        assert!(gateway.list().unwrap().is_empty());

        answering.store(true, Ordering::SeqCst);
        gateway.refresh().await.unwrap();
        let status = gateway.status();
        assert!(status.reachable);
        assert_eq!(status.error, None);
        let specs = gateway.list().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "fake");
        assert!(specs[0].gateway, "a gateway's spec says so");
        // The host alone: no port, path, query or token.
        assert_eq!(gateway.host(), addr.split(':').next().unwrap());
    }

    #[test]
    fn refreshes_sooner_while_the_gateway_isnt_answering() {
        let minute = Duration::from_secs(60);
        assert_eq!(next_refresh(true, minute), minute);
        assert_eq!(next_refresh(false, minute), RETRY);
        assert!(RETRY < minute);
        // Never slower than asked for.
        assert_eq!(
            next_refresh(false, Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn a_remotes_error_body_is_cut_to_its_first_line() {
        assert_eq!(brief("  asleep \n<html>...</html>"), "asleep");
        let long = "x".repeat(BRIEF + 50);
        let cut = brief(&long);
        assert_eq!(cut.chars().count(), BRIEF + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(brief(""), "");
    }

    #[tokio::test]
    async fn a_gateway_that_isnt_there_is_unreachable() {
        // Port 1 on loopback: nothing listens, and connecting is refused.
        let gateway = Gateway::new("http://127.0.0.1:1/", None).unwrap();
        assert!(gateway.refresh().await.is_err());
        let status = gateway.status();
        assert!(!status.reachable);
        let error = status.error.unwrap();
        // Says why, not only that the request failed.
        assert!(error.to_lowercase().contains("connect"), "{error}");
    }
}
