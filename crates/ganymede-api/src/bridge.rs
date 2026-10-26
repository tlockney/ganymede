//! One client's connection to a kernel: what a websocket carries.
//!
//! All clients share the kernel's sockets, so every message the kernel
//! sends is dispatched here to each client's own queue: iopub to everyone,
//! shell, control and stdin replies only to the client whose `session` is
//! in the parent header. `jupyter_server` gets the same effect by giving
//! each websocket its own DEALER sockets.
//!
//! As in `jupyter_server`'s `ZMQChannelsWebsocketConnection`, a client
//! names its session with the `session_id` query parameter, and:
//!
//! - when the last client of a kernel disconnects, its queue is kept and
//!   keeps filling ("buffering"). A client that connects with the same
//!   session id gets the queue, so it sees what the kernel sent while it
//!   was away; any other connection discards it.
//! - a new connection with the session id of one still open replaces it
//!   (the old one is closed), since that one is most likely a socket that
//!   died without a close, after a laptop slept.
//!
//! Queues are unbounded for replies, so a slow client never loses its own
//! `execute_reply`. Iopub messages beyond [`CLIENT_QUEUE_LIMIT`] queued are
//! dropped, and the client is told with a `stream` message on stderr, as
//! `jupyter_server` tells it when its iopub rate limit drops output.

use crate::kernels::RunningKernel;
use crate::KernelManager;
use ganymede_kernel_host::protocol::{Channel, Message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Notify};

/// Iopub messages queued for one client, connected or buffering, beyond
/// which further iopub messages are dropped. Replies are never dropped.
pub const CLIENT_QUEUE_LIMIT: usize = 10_000;

type Item = (Channel, Message);

/// One client's queue, as the kernel's dispatcher sees it.
struct Slot {
    /// The `session_id` the client connected with: the key for buffering
    /// and for replacing a stale connection.
    key: Option<String>,
    /// The client's `header.session`, learned from its first message.
    /// Replies are routed by it.
    session: Option<String>,
    tx: mpsc::UnboundedSender<Item>,
    queued: Arc<AtomicUsize>,
    /// Iopub messages dropped since the queue was last below the limit.
    dropped: usize,
    kick: Arc<Notify>,
}

impl Slot {
    fn wants(&self, channel: Channel, msg: &Message) -> bool {
        match channel {
            Channel::IoPub => true,
            _ => match (&self.session, msg.parent_header.as_ref()) {
                (Some(mine), Some(parent)) => &parent.session == mine,
                // Replies with no parent, or before this client has sent
                // anything, go to everyone rather than to no one.
                _ => true,
            },
        }
    }

    fn push(&self, item: Item) {
        if self.tx.send(item).is_ok() {
            self.queued.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// A disconnected session's queue, kept until it reconnects.
struct Parked {
    slot: u64,
    key: String,
    rx: mpsc::UnboundedReceiver<Item>,
}

/// Every client queue of one kernel. Outlives restarts, as the kernel's
/// clients do.
#[derive(Default)]
pub(crate) struct Clients {
    next: u64,
    slots: HashMap<u64, Slot>,
    parked: Option<Parked>,
    /// Connected clients, for the kernel model and the culler. A parked
    /// queue does not count.
    connections: usize,
}

impl Clients {
    pub(crate) fn connections(&self) -> usize {
        self.connections
    }

    /// Queue `msg` for every client that wants it.
    pub(crate) fn dispatch(&mut self, kernel_id: &str, channel: Channel, msg: &Message) {
        for slot in self.slots.values_mut() {
            if !slot.wants(channel, msg) {
                continue;
            }
            if channel == Channel::IoPub {
                if slot.queued.load(Ordering::SeqCst) >= CLIENT_QUEUE_LIMIT {
                    if slot.dropped == 0 {
                        tracing::warn!(
                            kernel = %kernel_id,
                            session = slot.key.as_deref().unwrap_or(""),
                            "client is {CLIENT_QUEUE_LIMIT} messages behind; dropping its output"
                        );
                    }
                    slot.dropped += 1;
                    continue;
                }
                if slot.dropped > 0 {
                    slot.push((Channel::IoPub, dropped_notice(slot.dropped, msg)));
                    slot.dropped = 0;
                }
            }
            slot.push((channel, msg.clone()));
        }
    }

    /// Forget a parked queue, as when the kernel is shut down.
    pub(crate) fn discard_parked(&mut self, kernel_id: &str) {
        if let Some(p) = self.parked.take() {
            if let Some(slot) = self.slots.remove(&p.slot) {
                let n = slot.queued.load(Ordering::SeqCst);
                if n > 0 {
                    tracing::info!(kernel = %kernel_id, session = %p.key, "discarding {n} buffered messages");
                }
            }
        }
    }
}

/// A `stream` message on stderr telling a client that `n` of its output
/// messages were dropped, with the parent of the message that follows.
fn dropped_notice(n: usize, next: &Message) -> Message {
    let mut m = Message::new(
        "stream",
        "server",
        "server",
        &serde_json::json!({
            "name": "stderr",
            "text": format!(
                "ganymede: {n} output messages were dropped because this client \
                 fell {CLIENT_QUEUE_LIMIT} messages behind.\n"
            ),
        }),
    );
    m.parent_header = next.parent_header.clone();
    m
}

pub struct ClientConnection {
    kernel: Arc<RunningKernel>,
    slot: u64,
    key: Option<String>,
    rx: mpsc::UnboundedReceiver<Item>,
    queued: Arc<AtomicUsize>,
    alive: watch::Receiver<bool>,
    kick: Arc<Notify>,
    replaced: bool,
    /// Whether the client's `header.session` has been recorded.
    session_known: bool,
}

impl KernelManager {
    /// A client with no session id: nothing is buffered for it.
    pub fn connect_client(&self, kernel_id: &str) -> crate::Result<ClientConnection> {
        self.connect_client_as(kernel_id, None)
    }

    /// A client for the websocket's `session_id` query parameter. It gets
    /// the queue buffered for that session since its last connection
    /// closed, and replaces a connection with that session still open.
    pub fn connect_client_as(
        &self,
        kernel_id: &str,
        session_id: Option<&str>,
    ) -> crate::Result<ClientConnection> {
        let kernel = self.get(kernel_id)?;
        let key = session_id.filter(|s| !s.is_empty()).map(str::to_owned);
        let mut clients = kernel.clients.lock().unwrap();
        // `jupyter_server`'s `get_buffer`: the buffer goes to its own
        // session and is discarded on any other connection.
        let resumed = match clients.parked.take() {
            Some(p) if key.as_deref() == Some(p.key.as_str()) => Some(p),
            Some(p) => {
                clients.parked = Some(p);
                clients.discard_parked(&kernel.id);
                None
            }
            None => None,
        };
        let resumed_slot = resumed.as_ref().map(|p| p.slot);
        if let Some(k) = &key {
            let stale = clients
                .slots
                .iter()
                .filter(|(id, s)| Some(**id) != resumed_slot && s.key.as_ref() == Some(k));
            for (_, slot) in stale {
                tracing::warn!(kernel = %kernel.id, session = %k, "replacing stale connection");
                slot.kick.notify_one();
            }
        }
        let (slot_id, rx, queued, kick) = match resumed {
            Some(p) => {
                let slot = clients.slots.get_mut(&p.slot).expect("parked slot");
                let kick = Arc::new(Notify::new());
                slot.kick = kick.clone();
                let queued = slot.queued.clone();
                tracing::info!(
                    kernel = %kernel.id,
                    session = %p.key,
                    "restoring connection, replaying {} buffered messages",
                    queued.load(Ordering::SeqCst)
                );
                (p.slot, p.rx, queued, kick)
            }
            None => {
                let (tx, rx) = mpsc::unbounded_channel();
                let queued = Arc::new(AtomicUsize::new(0));
                let kick = Arc::new(Notify::new());
                let id = clients.next;
                clients.next += 1;
                clients.slots.insert(
                    id,
                    Slot {
                        key: key.clone(),
                        session: None,
                        tx,
                        queued: queued.clone(),
                        dropped: 0,
                        kick: kick.clone(),
                    },
                );
                (id, rx, queued, kick)
            }
        };
        clients.connections += 1;
        drop(clients);
        let alive = kernel.watch_alive();
        Ok(ClientConnection {
            kernel,
            slot: slot_id,
            key,
            rx,
            queued,
            alive,
            kick,
            replaced: false,
            session_known: false,
        })
    }
}

impl ClientConnection {
    pub fn kernel_id(&self) -> &str {
        &self.kernel.id
    }

    /// Whether [`ClientConnection::recv`] ended because a newer connection
    /// with the same session id took over, rather than the kernel going.
    pub fn replaced(&self) -> bool {
        self.replaced
    }

    /// Forward a client's message to the kernel.
    pub async fn send(&mut self, channel: Channel, msg: Message) -> crate::Result<()> {
        if !self.session_known {
            self.session_known = true;
            let mut clients = self.kernel.clients.lock().unwrap();
            if let Some(slot) = clients.slots.get_mut(&self.slot) {
                slot.session = Some(msg.header.session.clone());
            }
        }
        if channel == Channel::IoPub {
            return Err(crate::ApiError::bad_request("clients cannot send on iopub"));
        }
        self.kernel.wait_booted().await;
        self.kernel.touch();
        if channel == Channel::Shell {
            self.kernel.sent_on_shell(&msg.header);
        }
        if msg.msg_type() == "shutdown_request" {
            self.kernel.asked_to_exit();
        }
        self.kernel.session().send(channel, msg).await?;
        Ok(())
    }

    /// The next message for this client. `None` when the kernel is gone
    /// or this connection was [replaced](ClientConnection::replaced).
    pub async fn recv(&mut self) -> Option<(Channel, Message)> {
        loop {
            if self.replaced {
                return None;
            }
            if !*self.alive.borrow() {
                // Deliver what is already queued, such as the
                // `status: dead` announced just before the kernel closed.
                return self.rx.try_recv().ok().inspect(|_| self.took());
            }
            tokio::select! {
                biased;
                got = self.rx.recv() => {
                    if got.is_some() {
                        self.took();
                    }
                    return got;
                }
                _ = self.kick.notified() => self.replaced = true,
                _ = self.alive.changed() => {}
            }
        }
    }

    fn took(&self) {
        self.queued.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for ClientConnection {
    fn drop(&mut self) {
        let mut clients = self.kernel.clients.lock().unwrap();
        clients.connections -= 1;
        let park = clients.connections == 0 && !self.replaced && self.kernel.is_alive();
        match (&self.key, park) {
            // `jupyter_server`'s `start_buffering`: the last connection of
            // a kernel keeps its queue for the session to come back to.
            (Some(key), true) => {
                clients.discard_parked(&self.kernel.id);
                tracing::info!(kernel = %self.kernel.id, session = %key, "buffering messages");
                let (_, placeholder) = mpsc::unbounded_channel();
                clients.parked = Some(Parked {
                    slot: self.slot,
                    key: key.clone(),
                    rx: std::mem::replace(&mut self.rx, placeholder),
                });
            }
            _ => {
                clients.slots.remove(&self.slot);
            }
        }
    }
}

// Re-export so the HTTP layer needs only this crate.
pub use ganymede_kernel_host::KernelSession;

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(clients: &mut Clients, session: &str) -> mpsc::UnboundedReceiver<Item> {
        let (tx, rx) = mpsc::unbounded_channel();
        clients.slots.insert(
            clients.next,
            Slot {
                key: None,
                session: Some(session.into()),
                tx,
                queued: Arc::new(AtomicUsize::new(0)),
                dropped: 0,
                kick: Arc::new(Notify::new()),
            },
        );
        clients.next += 1;
        rx
    }

    #[test]
    fn a_full_queue_drops_output_but_never_replies() {
        let mut clients = Clients::default();
        let mut rx = slot(&mut clients, "me");
        let request = Message::new("execute_request", "me", "u", &serde_json::json!({}));
        let output = Message::reply_to(&request, "stream", &serde_json::json!({"text": "x"}));
        let reply = Message::reply_to(&request, "execute_reply", &serde_json::json!({}));
        for _ in 0..CLIENT_QUEUE_LIMIT + 5 {
            clients.dispatch("k", Channel::IoPub, &output);
        }
        clients.dispatch("k", Channel::Shell, &reply);
        let mut got = vec![];
        while let Ok(item) = rx.try_recv() {
            got.push(item);
        }
        assert_eq!(got.len(), CLIENT_QUEUE_LIMIT + 1);
        assert_eq!(got.last().unwrap().1.msg_type(), "execute_reply");

        // Once the client catches up, it is told what it missed.
        let slot = clients.slots.values().next().unwrap();
        slot.queued.store(0, Ordering::SeqCst);
        clients.dispatch("k", Channel::IoPub, &output);
        let (ch, notice) = rx.try_recv().unwrap();
        assert_eq!((ch, notice.msg_type()), (Channel::IoPub, "stream"));
        assert_eq!(notice.content["name"], "stderr");
        assert!(notice.content["text"]
            .as_str()
            .unwrap()
            .contains("5 output messages"));
        assert_eq!(notice.parent_id(), Some(request.msg_id()));
        assert_eq!(rx.try_recv().unwrap().1.msg_type(), "stream");
    }

    #[test]
    fn replies_go_to_their_own_session() {
        let mut clients = Clients::default();
        let mut mine = slot(&mut clients, "me");
        let mut theirs = slot(&mut clients, "them");
        let request = Message::new("execute_request", "me", "u", &serde_json::json!({}));
        let reply = Message::reply_to(&request, "execute_reply", &serde_json::json!({}));
        clients.dispatch("k", Channel::Shell, &reply);
        assert!(mine.try_recv().is_ok());
        assert!(theirs.try_recv().is_err());
    }
}
