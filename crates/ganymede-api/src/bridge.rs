//! One client's connection to a kernel: what a websocket carries.
//!
//! All clients share the kernel's sockets, so the kernel's replies on
//! shell, control, and stdin are broadcast to every client here and
//! filtered by the `session` the client put in its request headers.
//! `jupyter_server` gets the same effect by giving each websocket its own
//! DEALER sockets. iopub goes to everyone.

use crate::kernels::RunningKernel;
use crate::KernelManager;
use ganymede_kernel_host::protocol::{Channel, Message};
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct ClientConnection {
    kernel: Arc<RunningKernel>,
    rx: broadcast::Receiver<(Channel, Message)>,
    alive: tokio::sync::watch::Receiver<bool>,
    /// The client's `header.session`, learned from its first message.
    client_session: Option<String>,
}

impl KernelManager {
    pub fn connect_client(&self, kernel_id: &str) -> crate::Result<ClientConnection> {
        let kernel = self.get(kernel_id)?;
        kernel.add_connection();
        let rx = kernel.subscribe();
        let alive = kernel.watch_alive();
        Ok(ClientConnection {
            kernel,
            rx,
            alive,
            client_session: None,
        })
    }
}

impl ClientConnection {
    pub fn kernel_id(&self) -> &str {
        &self.kernel.id
    }

    /// Forward a client's message to the kernel.
    pub async fn send(&mut self, channel: Channel, msg: Message) -> crate::Result<()> {
        if self.client_session.is_none() {
            self.client_session = Some(msg.header.session.clone());
        }
        if channel == Channel::IoPub {
            return Err(crate::ApiError::bad_request("clients cannot send on iopub"));
        }
        self.kernel.touch();
        self.kernel.session().send(channel, msg).await?;
        Ok(())
    }

    /// The next message for this client. `None` when the kernel is gone.
    pub async fn recv(&mut self) -> Option<(Channel, Message)> {
        loop {
            if !*self.alive.borrow() {
                return None;
            }
            let got = tokio::select! {
                got = self.rx.recv() => got,
                _ = self.alive.changed() => continue,
            };
            match got {
                Ok((channel, msg)) => {
                    if self.wants(channel, &msg) {
                        return Some((channel, msg));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(kernel = %self.kernel.id, "client lagged by {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    fn wants(&self, channel: Channel, msg: &Message) -> bool {
        match channel {
            Channel::IoPub => true,
            _ => match (&self.client_session, msg.parent_header.as_ref()) {
                (Some(mine), Some(parent)) => &parent.session == mine,
                // Replies with no parent, or before this client has sent
                // anything, go to everyone rather than to no one.
                _ => true,
            },
        }
    }
}

impl Drop for ClientConnection {
    fn drop(&mut self) {
        self.kernel.remove_connection();
    }
}

// Re-export so the HTTP layer needs only this crate.
pub use ganymede_kernel_host::KernelSession;
