//! Jupyter messaging protocol, version 5.5, with no I/O.
//!
//! This crate holds the message envelope ([`Message`], [`Header`]), typed
//! content for the messages a host needs ([`content`]), the ZeroMQ multipart
//! framing with HMAC signing ([`wire`]), the connection file
//! ([`ConnectionInfo`]), and the two websocket codecs a Jupyter server
//! speaks ([`ws`]).
//!
//! Design rules:
//! - Unknown fields are preserved everywhere via `#[serde(flatten)]`, so a
//!   kernel's extra `kernel_info_reply` fields (capability manifests, for
//!   example) pass through a host untouched.
//! - The host knows no language. Nothing here special-cases any kernel.
//! - Nothing here does network or file I/O; transports live in other crates.

pub mod connection;
pub mod content;
pub mod message;
pub mod wire;
pub mod ws;

pub use connection::ConnectionInfo;
pub use message::{Channel, Header, Message, PROTOCOL_VERSION};

/// Errors from encoding or decoding messages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed wire message: {0}")]
    Malformed(&'static str),
    #[error("bad signature")]
    BadSignature,
    #[error("unsupported signature scheme: {0}")]
    UnsupportedScheme(String),
    #[error("unknown channel: {0}")]
    UnknownChannel(String),
}

pub type Result<T> = std::result::Result<T, Error>;
