//! ZeroMQ transport over the pure-Rust `zeromq` crate.
//!
//! Host-side sockets: DEALER for shell, control, and stdin; SUB for iopub;
//! REQ for heartbeat. One task per socket owns it.

use crate::transport::{Incoming, KernelTransport, TransportSink};
use crate::{Error, Result};
use async_trait::async_trait;
use bytes::Bytes;
use ganymede_protocol::wire::{self, DigestHistory, Signer};
use ganymede_protocol::{Channel, ConnectionInfo, Message};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Mutex};
use zeromq::util::PeerIdentity;
use zeromq::{Socket, SocketRecv, SocketSend, ZmqMessage};

fn zerr(e: zeromq::ZmqError) -> Error {
    Error::Zmq(e.to_string())
}

pub struct ZmqTransport {
    outgoing: std::collections::HashMap<Channel, mpsc::Sender<Message>>,
    hb: Mutex<HeartbeatSocket>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

struct HeartbeatSocket {
    endpoint: String,
    socket: Option<zeromq::ReqSocket>,
}

impl HeartbeatSocket {
    async fn socket(&mut self) -> Result<&mut zeromq::ReqSocket> {
        if self.socket.is_none() {
            let mut s = zeromq::ReqSocket::new();
            s.connect(&self.endpoint).await.map_err(zerr)?;
            self.socket = Some(s);
        }
        Ok(self.socket.as_mut().unwrap())
    }
}

impl ZmqTransport {
    /// Start connecting all five sockets and return at once: each socket's
    /// task keeps retrying until the kernel accepts it, and messages wait
    /// until then. Whether the kernel is up is the session's nudge to find
    /// out, under its timeout.
    pub async fn connect(info: &ConnectionInfo) -> Result<KernelTransport> {
        let signer = info.signer()?;
        let history = Arc::new(DigestHistory::default());
        let (in_tx, in_rx) = mpsc::channel::<(Channel, Message)>(1024);
        let mut outgoing = std::collections::HashMap::new();
        let mut tasks = Vec::new();
        // One routing identity for shell, control, and stdin, as
        // jupyter_client does: a kernel sends `input_request` on its stdin
        // ROUTER to the identity that sent the `execute_request` on shell,
        // so a stdin socket with its own identity never receives it.
        let identity: PeerIdentity = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .parse()
            .map_err(zerr)?;

        // Shell sends nothing until stdin and iopub are connected, so the
        // kernel can route an `input_request` and we see the outputs of the
        // first request.
        let (stdin_ready, stdin_rx) = watch::channel(false);
        let (iopub_ready, iopub_rx) = watch::channel(false);
        for (channel, endpoint, wait, ready) in [
            (
                Channel::Shell,
                info.shell_endpoint(),
                vec![stdin_rx, iopub_rx],
                None,
            ),
            (Channel::Control, info.control_endpoint(), vec![], None),
            (
                Channel::Stdin,
                info.stdin_endpoint(),
                vec![],
                Some(stdin_ready),
            ),
        ] {
            let (out_tx, out_rx) = mpsc::channel::<Message>(256);
            outgoing.insert(channel, out_tx);
            tasks.push(tokio::spawn(dealer_loop(
                Endpoint {
                    channel,
                    endpoint,
                    wait,
                    ready,
                },
                identity.clone(),
                signer.clone(),
                history.clone(),
                out_rx,
                in_tx.clone(),
            )));
        }

        tasks.push(tokio::spawn(sub_loop(
            info.iopub_endpoint(),
            iopub_ready,
            signer.clone(),
            history,
            in_tx.clone(),
        )));

        let hb = Mutex::new(HeartbeatSocket {
            endpoint: info.hb_endpoint(),
            socket: None,
        });
        let sink = Arc::new(ZmqTransport {
            outgoing,
            hb,
            tasks,
        });
        Ok(KernelTransport {
            sink,
            incoming: in_rx as Incoming,
        })
    }
}

/// Wait until something accepts TCP connections on `endpoint`. zeromq
/// retries a refused connection itself, but backs off to 5 s between
/// tries, and a ZMTP handshake cut short by a timeout can leave the
/// kernel's ROUTER holding our routing id, so later connections with it
/// are ignored. A plain TCP probe, retried quickly, finds the moment the
/// kernel binds without either problem.
async fn wait_listening(endpoint: &str) {
    let Some(addr) = endpoint.strip_prefix("tcp://") else {
        return;
    };
    let mut delay = Duration::from_millis(20);
    while tokio::net::TcpStream::connect(addr).await.is_err() {
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(250));
    }
}

/// Connect a socket, retrying until the ZMTP handshake completes. Until
/// the kernel binds its port, the port can briefly belong to another
/// socket (another host probing for free ports, another kernel), so a
/// failed handshake is retried like a refused connection.
async fn connect_retrying<S: Socket>(endpoint: &str, new: impl Fn() -> S) -> S {
    loop {
        wait_listening(endpoint).await;
        let mut socket = new();
        match socket.connect(endpoint).await {
            Ok(()) => return socket,
            Err(e) => {
                tracing::debug!(endpoint, "zmq connect failed, retrying: {e}");
                let _ = socket.close().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn connect_dealer(endpoint: &str, identity: &PeerIdentity) -> zeromq::DealerSocket {
    connect_retrying(endpoint, || {
        let mut options = zeromq::SocketOptions::default();
        options.peer_identity(identity.clone());
        zeromq::DealerSocket::with_options(options)
    })
    .await
}

async fn connect_sub(endpoint: &str) -> zeromq::SubSocket {
    connect_retrying(endpoint, zeromq::SubSocket::new).await
}

struct Dealer {
    channel: Channel,
    endpoint: String,
    identity: PeerIdentity,
    socket: zeromq::DealerSocket,
}

impl Dealer {
    /// zeromq reconnects SUB sockets but not DEALERs: once the peer goes
    /// away (a port held by the wrong socket, a kernel restarting on the
    /// same ports), the socket stays unconnected. Replace it.
    async fn reconnect(&mut self) {
        let fresh = connect_dealer(&self.endpoint, &self.identity).await;
        let old = std::mem::replace(&mut self.socket, fresh);
        let _ = old.close().await;
    }

    async fn send(&mut self, zmsg: ZmqMessage) {
        if let Err(e) = self.socket.send(zmsg.clone()).await {
            tracing::debug!(channel = %self.channel, "zmq send failed, reconnecting: {e}");
            self.reconnect().await;
            if let Err(e) = self.socket.send(zmsg).await {
                tracing::warn!(channel = %self.channel, "zmq send failed: {e}");
            }
        }
    }
}

/// Where a DEALER connects, which sockets it waits for before sending, and
/// what it signals once connected.
struct Endpoint {
    channel: Channel,
    endpoint: String,
    wait: Vec<watch::Receiver<bool>>,
    ready: Option<watch::Sender<bool>>,
}

async fn dealer_loop(
    Endpoint {
        channel,
        endpoint,
        wait,
        ready,
    }: Endpoint,
    identity: PeerIdentity,
    signer: Signer,
    history: Arc<DigestHistory>,
    mut out_rx: mpsc::Receiver<Message>,
    in_tx: mpsc::Sender<(Channel, Message)>,
) {
    // Messages queue in `out_rx` until the first connection is made.
    let socket = connect_dealer(&endpoint, &identity).await;
    if let Some(ready) = ready {
        ready.send_replace(true);
    }
    for mut rx in wait {
        let _ = rx.wait_for(|connected| *connected).await;
    }
    let mut dealer = Dealer {
        channel,
        endpoint,
        identity,
        socket,
    };
    loop {
        // `recv` polls a stream and `out_rx.recv` is cancel-safe, so a
        // select over both does not lose frames.
        tokio::select! {
            out = out_rx.recv() => {
                let Some(msg) = out else { break };
                match wire::encode(&signer, &[], &msg) {
                    Ok(frames) => {
                        let zmsg = ZmqMessage::try_from(frames).expect("non-empty frames");
                        dealer.send(zmsg).await;
                    }
                    Err(e) => tracing::warn!(%channel, "encode failed: {e}"),
                }
            }
            got = dealer.socket.recv() => {
                match got {
                    Ok(zmsg) => {
                        let frames: Vec<Bytes> = zmsg.into_vec();
                        match wire::decode_once(&signer, &history, &frames) {
                            Ok(w) => {
                                if in_tx.send((channel, w.message)).await.is_err() { break }
                            }
                            Err(e) => tracing::warn!(%channel, "dropping message: {e}"),
                        }
                    }
                    Err(e) => { tracing::warn!(%channel, "zmq recv failed: {e}"); break }
                }
            }
        }
    }
    let _ = dealer.socket.close().await;
}

async fn sub_loop(
    endpoint: String,
    ready: watch::Sender<bool>,
    signer: Signer,
    history: Arc<DigestHistory>,
    in_tx: mpsc::Sender<(Channel, Message)>,
) {
    let mut socket = connect_sub(&endpoint).await;
    if let Err(e) = socket.subscribe("").await {
        tracing::warn!("iopub subscribe failed: {e}");
        return;
    }
    ready.send_replace(true);
    loop {
        match socket.recv().await {
            Ok(zmsg) => {
                let frames: Vec<Bytes> = zmsg.into_vec();
                match wire::decode_once(&signer, &history, &frames) {
                    Ok(w) => {
                        if in_tx.send((Channel::IoPub, w.message)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("dropping iopub message: {e}"),
                }
            }
            Err(e) => {
                tracing::warn!("iopub recv failed: {e}");
                break;
            }
        }
    }
    let _ = socket.close().await;
}

/// A transport dropped without `close`, such as a session whose kernel
/// died, must not leave its socket tasks reconnecting for ever.
impl Drop for ZmqTransport {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[async_trait]
impl TransportSink for ZmqTransport {
    async fn send(&self, channel: Channel, msg: Message) -> Result<()> {
        let tx = self
            .outgoing
            .get(&channel)
            .ok_or(Error::Unsupported("iopub is receive-only on the host side"))?;
        tx.send(msg).await.map_err(|_| Error::TransportClosed)
    }

    async fn heartbeat(&self, timeout: Duration) -> Result<bool> {
        let mut hb = self.hb.lock().await;
        let socket = hb.socket().await?;
        socket.send(ZmqMessage::from("ping")).await.map_err(zerr)?;
        match tokio::time::timeout(timeout, socket.recv()).await {
            Ok(Ok(_)) => Ok(true),
            Ok(Err(e)) => Err(zerr(e)),
            Err(_) => {
                // A REQ socket that never got its reply is stuck; replace it.
                if let Some(s) = hb.socket.take() {
                    let _ = s.close().await;
                }
                Ok(false)
            }
        }
    }

    async fn close(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeromq::RouterSocket;

    /// The port a socket bound to `tcp://127.0.0.1:0` got. Letting the OS
    /// pick on bind cannot collide with a parallel test, unlike probing for
    /// a free port and binding it later.
    fn port(e: zeromq::Endpoint) -> u16 {
        match e {
            zeromq::Endpoint::Tcp(_, port) => port,
            other => panic!("not a TCP endpoint: {other}"),
        }
    }

    async fn first_frame(router: &mut RouterSocket) -> Bytes {
        let msg = tokio::time::timeout(Duration::from_secs(5), router.recv())
            .await
            .expect("router received nothing")
            .unwrap();
        msg.get(0).unwrap().clone()
    }

    /// A kernel routes `input_request` to the identity that sent the
    /// `execute_request`, so shell and stdin must share it.
    #[tokio::test]
    async fn shell_and_stdin_share_a_routing_identity() {
        let any = "tcp://127.0.0.1:0";
        let mut shell = RouterSocket::new();
        let mut stdin = RouterSocket::new();
        let mut control = RouterSocket::new();
        let mut iopub = zeromq::PubSocket::new();
        let mut hb = zeromq::RepSocket::new();
        let ports = [
            port(shell.bind(any).await.unwrap()),
            port(iopub.bind(any).await.unwrap()),
            port(stdin.bind(any).await.unwrap()),
            port(control.bind(any).await.unwrap()),
            port(hb.bind(any).await.unwrap()),
        ];
        let info = ConnectionInfo::new_tcp("127.0.0.1", ports);

        let transport = ZmqTransport::connect(&info).await.unwrap();
        let msg = Message::new("kernel_info_request", "s", "u", &serde_json::json!({}));
        transport
            .sink
            .send(Channel::Shell, msg.clone())
            .await
            .unwrap();
        transport.sink.send(Channel::Stdin, msg).await.unwrap();

        let a = first_frame(&mut shell).await;
        let b = first_frame(&mut stdin).await;
        assert_eq!(a, b);
        transport.sink.close().await;
    }
}
