//! The server API as plain functions over the kernel host.
//!
//! Nothing here knows about HTTP. [`KernelManager`] and [`SessionManager`]
//! return the JSON models `jupyter_server` returns, and [`ClientConnection`]
//! is one websocket's worth of kernel traffic. The HTTP server maps these
//! to routes and frames; an in-app bridge can call them directly.

pub mod bridge;
pub mod kernel_log;
pub mod kernels;
pub mod models;
pub mod sessions;

pub use bridge::ClientConnection;
pub use kernel_log::{KernelLogLine, KernelLogModel};
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
            E::SendTimeout => ApiError::new(504, e.to_string()),
            E::KernelDied | E::TransportClosed => ApiError::new(503, e.to_string()),
            other => ApiError::new(500, other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Now, in the timestamp format `jupyter_server` uses.
pub fn now_iso() -> String {
    iso_micros(jiff::Timestamp::now())
}

/// `t` as `YYYY-MM-DDTHH:MM:SS.ffffffZ`: UTC, six fraction digits
/// (truncated, not rounded), `Z`.
fn iso_micros(t: jiff::Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%S.%6fZ").to_string()
}

#[cfg(test)]
mod iso_tests {
    use super::*;

    /// Fixed instants, so the format is pinned independent of the clock. The
    /// expected strings are what chrono's formatting gave before the port.
    #[test]
    fn format_is_pinned() {
        // 2026-01-02T03:04:05Z
        let at = |nanos: i32| jiff::Timestamp::new(1_767_323_045, nanos).unwrap();
        for (nanos, want) in [
            (0, "2026-01-02T03:04:05.000000Z"),
            (123000000, "2026-01-02T03:04:05.123000Z"),
            (123456000, "2026-01-02T03:04:05.123456Z"),
            (123456789, "2026-01-02T03:04:05.123456Z"),
            (1000, "2026-01-02T03:04:05.000001Z"),
            (100, "2026-01-02T03:04:05.000000Z"),
            (999999999, "2026-01-02T03:04:05.999999Z"),
        ] {
            assert_eq!(iso_micros(at(nanos)), want, "nanos {nanos}");
        }
    }
}
