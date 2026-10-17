//! Kernel lifecycle for a language-agnostic Jupyter host.
//!
//! Seams, each a trait with at least one implementation here:
//! - [`transport::TransportSink`] plus an incoming stream: a stream of
//!   channel messages to and from one kernel. [`zmq::ZmqTransport`] is the
//!   ZeroMQ implementation; [`transport::in_process_pair`] is the in-process
//!   one an app uses when it owns the kernel.
//! - [`process::Provisioner`]: how a kernel comes to exist.
//!   [`process::SubprocessProvisioner`] launches from a kernelspec;
//!   [`process::attach`] uses a kernel that already runs.
//! - [`kernelspec::KernelspecSource`]: where launchable kernels are listed.
//!   [`kernelspec::FsKernelspecs`] reads the Jupyter data directories.
//!
//! [`session::KernelSession`] is the state machine on top: the startup nudge,
//! busy/idle tracking, reply routing by parent id, the two-part completion
//! rule (reply and idle), fan-out to many clients, interrupt and shutdown.
//!
//! The host knows no language. Nothing here reads `language` except to
//! report it.

pub mod kernelspec;
pub mod process;
pub mod session;
pub mod testing;
pub mod transport;
pub mod zmq;

pub use ganymede_protocol as protocol;
pub use kernelspec::{FsKernelspecs, InterruptMode, Kernelspec, KernelspecSource};
pub use process::{attach, LaunchOptions, LaunchedKernel, Provisioner, SubprocessProvisioner};
pub use session::{Execution, KernelSession};
pub use transport::{in_process_pair, KernelSide, KernelTransport, TransportSink};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("protocol: {0}")]
    Protocol(#[from] ganymede_protocol::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zmq: {0}")]
    Zmq(String),
    #[error("kernelspec not found: {0}")]
    NoSuchKernelspec(String),
    #[error("invalid kernelspec at {path}: {reason}")]
    InvalidKernelspec {
        path: std::path::PathBuf,
        reason: String,
    },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("kernel died")]
    KernelDied,
    #[error("transport closed")]
    TransportClosed,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
