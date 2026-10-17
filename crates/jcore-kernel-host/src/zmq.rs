//! ZeroMQ transport over the pure-Rust `zeromq` crate.
//!
//! Host-side sockets: DEALER for shell, control, and stdin; SUB for iopub;
//! REQ for heartbeat. One task per socket owns it.

use crate::transport::{Incoming, KernelTransport, TransportSink};
use crate::{Error, Result};
use async_trait::async_trait;
use bytes::Bytes;
use jcore_protocol::wire::{self, Signer};
use jcore_protocol::{Channel, ConnectionInfo, Message};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
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
    /// Connect all five sockets. Returns once connected; the kernel need
    /// not be ready yet, which is what the session's nudge is for.
    pub async fn connect(info: &ConnectionInfo) -> Result<KernelTransport> {
        let signer = info.signer()?;
        let (in_tx, in_rx) = mpsc::channel::<(Channel, Message)>(1024);
        let mut outgoing = std::collections::HashMap::new();
        let mut tasks = Vec::new();

        for (channel, endpoint) in [
            (Channel::Shell, info.shell_endpoint()),
            (Channel::Control, info.control_endpoint()),
            (Channel::Stdin, info.stdin_endpoint()),
        ] {
            let mut socket = zeromq::DealerSocket::new();
            socket.connect(&endpoint).await.map_err(zerr)?;
            let (out_tx, out_rx) = mpsc::channel::<Message>(256);
            outgoing.insert(channel, out_tx);
            tasks.push(tokio::spawn(dealer_loop(
                channel,
                socket,
                signer.clone(),
                out_rx,
                in_tx.clone(),
            )));
        }

        let mut sub = zeromq::SubSocket::new();
        sub.connect(&info.iopub_endpoint()).await.map_err(zerr)?;
        sub.subscribe("").await.map_err(zerr)?;
        tasks.push(tokio::spawn(sub_loop(sub, signer.clone(), in_tx.clone())));

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

async fn dealer_loop(
    channel: Channel,
    mut socket: zeromq::DealerSocket,
    signer: Signer,
    mut out_rx: mpsc::Receiver<Message>,
    in_tx: mpsc::Sender<(Channel, Message)>,
) {
    loop {
        // `recv` polls a stream and `out_rx.recv` is cancel-safe, so a
        // select over both does not lose frames.
        tokio::select! {
            out = out_rx.recv() => {
                let Some(msg) = out else { break };
                match wire::encode(&signer, &[], &msg) {
                    Ok(frames) => {
                        let zmsg = ZmqMessage::try_from(frames).expect("non-empty frames");
                        if let Err(e) = socket.send(zmsg).await {
                            tracing::warn!(%channel, "zmq send failed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!(%channel, "encode failed: {e}"),
                }
            }
            got = socket.recv() => {
                match got {
                    Ok(zmsg) => {
                        let frames: Vec<Bytes> = zmsg.into_vec();
                        match wire::decode(&signer, &frames) {
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
    let _ = socket.close().await;
}

async fn sub_loop(
    mut socket: zeromq::SubSocket,
    signer: Signer,
    in_tx: mpsc::Sender<(Channel, Message)>,
) {
    loop {
        match socket.recv().await {
            Ok(zmsg) => {
                let frames: Vec<Bytes> = zmsg.into_vec();
                match wire::decode(&signer, &frames) {
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
