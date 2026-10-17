//! The transport seam: a stream of channel messages to and from one kernel.

use crate::Result;
use async_trait::async_trait;
use jcore_protocol::{Channel, Message};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Messages arriving from the kernel, tagged with their channel.
pub type Incoming = mpsc::Receiver<(Channel, Message)>;

/// The outbound half of a transport.
#[async_trait]
pub trait TransportSink: Send + Sync {
    async fn send(&self, channel: Channel, msg: Message) -> Result<()>;
    /// True if the kernel answered a heartbeat within `timeout`.
    async fn heartbeat(&self, timeout: Duration) -> Result<bool>;
    async fn close(&self);
}

/// A connected transport: where to send, and where messages arrive.
pub struct KernelTransport {
    pub sink: Arc<dyn TransportSink>,
    pub incoming: Incoming,
}

/// The kernel's end of an in-process transport. A kernel implementation
/// (or a test fake) reads requests from `incoming` and writes replies and
/// iopub traffic to `outgoing`.
pub struct KernelSide {
    pub incoming: Incoming,
    pub outgoing: mpsc::Sender<(Channel, Message)>,
}

struct InProcessSink {
    to_kernel: mpsc::Sender<(Channel, Message)>,
}

#[async_trait]
impl TransportSink for InProcessSink {
    async fn send(&self, channel: Channel, msg: Message) -> Result<()> {
        self.to_kernel
            .send((channel, msg))
            .await
            .map_err(|_| crate::Error::TransportClosed)
    }
    async fn heartbeat(&self, _timeout: Duration) -> Result<bool> {
        Ok(!self.to_kernel.is_closed())
    }
    async fn close(&self) {}
}

/// An in-process transport pair. The host side is a [`KernelTransport`]
/// like any other; the kernel side is handed to whatever runs cells.
pub fn in_process_pair(capacity: usize) -> (KernelTransport, KernelSide) {
    let (to_kernel, kernel_incoming) = mpsc::channel(capacity);
    let (to_host, host_incoming) = mpsc::channel(capacity);
    (
        KernelTransport {
            sink: Arc::new(InProcessSink { to_kernel }),
            incoming: host_incoming,
        },
        KernelSide {
            incoming: kernel_incoming,
            outgoing: to_host,
        },
    )
}
