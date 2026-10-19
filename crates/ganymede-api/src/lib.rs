//! The server API as plain functions over the kernel host.
//!
//! Nothing here knows about HTTP. [`KernelManager`] and [`SessionManager`]
//! return the JSON models `jupyter_server` returns, and [`ClientConnection`]
//! is one websocket's worth of kernel traffic. The HTTP server maps these
//! to routes and frames; an in-app bridge can call them directly.

pub mod bridge;
pub mod kernels;
pub mod models;
pub mod sessions;

pub use bridge::ClientConnection;
#[cfg(feature = "zmq")]
pub use kernels::ZmqTransportFactory;
pub use kernels::{
    KernelManager, KernelManagerConfig, RunningKernel, StaticKernelspecs, TransportFactory,
};
pub use models::*;
pub use sessions::{SessionCreate, SessionManager, SessionPatch};

/// An API error with the HTTP status `jupyter_server` would use.
#[derive(Debug, thiserror::Error)]
#[error("{status}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(404, format!("{what} not found"))
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
}

impl From<ganymede_kernel_host::Error> for ApiError {
    fn from(e: ganymede_kernel_host::Error) -> Self {
        use ganymede_kernel_host::Error as E;
        match e {
            E::NoSuchKernelspec(n) => ApiError::new(404, format!("No such kernel named {n}")),
            E::Timeout(what) => ApiError::new(504, format!("timed out waiting for {what}")),
            E::KernelDied | E::TransportClosed => ApiError::new(503, e.to_string()),
            other => ApiError::new(500, other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Now, in the timestamp format `jupyter_server` uses.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
