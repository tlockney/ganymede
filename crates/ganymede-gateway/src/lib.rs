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

/// Errors from the remote server, in kernel-host's error type.
fn remote(what: impl std::fmt::Display) -> Error {
    Error::Io(std::io::Error::other(format!("gateway: {what}")))
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
    /// Remote kernel ids by the address of the process handle in the
    /// `LaunchedKernel` that [`Provisioner::launch`] returned, which is how
    /// [`TransportFactory::connect`] finds the kernel to connect to.
    kernels: Mutex<HashMap<usize, String>>,
}

fn handle_key(process: &Arc<dyn KernelProcess>) -> usize {
    Arc::as_ptr(process) as *const () as usize
}

impl Gateway {
    /// A gateway to the server at `url` (`http(s)://host[:port][/base]`),
    /// authenticating with `token` when given. No request is made yet.
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
                kernels: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Fetches the remote kernelspecs into the cache [`KernelspecSource::list`]
    /// reads. Until the first success the list is empty.
    pub async fn refresh(&self) -> Result<()> {
        let v: Value = self.inner.get("api/kernelspecs").await?;
        let mut specs = Vec::new();
        if let Some(map) = v.get("kernelspecs").and_then(Value::as_object) {
            for (name, entry) in map {
                let Some(spec) = entry.get("spec") else {
                    continue;
                };
                match serde_json::from_value::<Kernelspec>(spec.clone()) {
                    Ok(mut spec) => {
                        spec.name = name.clone();
                        specs.push(spec);
                    }
                    Err(e) => tracing::warn!("gateway kernelspec {name}: {e}"),
                }
            }
        }
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        *self.inner.specs.write().unwrap() = specs;
        Ok(())
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

    /// Calls [`Gateway::refresh`] now and then every `every`, logging
    /// failures, for as long as the gateway exists.
    pub fn refresh_every(&self, every: Duration) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return };
                if let Err(e) = (Gateway { inner }).refresh().await {
                    tracing::warn!("{e}");
                }
                tokio::time::sleep(every).await;
            }
        });
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
        let mut req = self.http.request(method, self.url(path)?);
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("token {t}"));
        }
        Ok(req)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = req.send().await.map_err(remote)?;
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
        Err(remote(format!("{url}: {status} {reason}")))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let resp = self.send(self.request(reqwest::Method::GET, path)?).await?;
        resp.json().await.map_err(remote)
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
        let result = self.inner.send(req).await.map(drop);
        let _ = self.alive_tx.send(false);
        result
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
        let _ = self.alive_tx.send(false);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if let Ok(req) = self.inner.request(reqwest::Method::DELETE, &self.path("")) {
            let inner = self.inner.clone();
            handle.spawn(async move {
                let _ = inner.send(req).await;
            });
        }
    }
}

impl Drop for RemoteKernel {
    fn drop(&mut self) {
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
