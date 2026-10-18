//! Jupyter notebook file format (nbformat v4) for Ganymede.
//!
//! - [`Notebook`], [`Cell`], [`Output`] and [`MimeBundle`] model nbformat 4.x
//!   (minor 5). Unknown fields are kept in `extra` maps so nothing is lost on
//!   a round trip.
//! - Reading ([`Notebook::from_str`], [`Notebook::from_reader`],
//!   [`read_file`]) is lenient, like Python nbformat, which only logs
//!   validation errors: multi-line text may be a string or an array of
//!   strings, `null`s and missing output fields read as empty or default,
//!   outputs that still do not fit are kept verbatim as [`Output::Unknown`],
//!   and lone surrogate escapes read as U+FFFD. Transient metadata is
//!   stripped, and for nbformat 4.5 missing or duplicate cell ids are
//!   repaired (on write too), as Python nbformat does.
//! - Writing ([`Notebook::to_string`](ToString::to_string),
//!   [`Notebook::to_file_string`], [`write_file`]) reproduces Python
//!   nbformat's output byte for byte, so a notebook saved by this crate
//!   produces no spurious git diff against one saved by Jupyter.
//!
//! The crate does no I/O beyond the `read_file`/`write_file` conveniences
//! and [`atomic_write`], which `write_file` uses.

mod atomic;
mod multiline;
mod reader;
mod types;
mod writer;

pub use atomic::atomic_write;
pub use multiline::{splitlines, MultilineString};
pub use reader::read_file;
pub use types::{
    is_json_mime, is_split_mime, Attachments, Cell, CodeCell, DisplayDataOutput, ErrorOutput,
    ExecuteResultOutput, MarkdownCell, MimeBundle, Notebook, Output, RawCell, StreamOutput,
    NBFORMAT, NBFORMAT_MINOR,
};
pub use writer::{python_float_repr, write_file};

/// Errors from reading or writing notebooks.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid notebook JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("notebook JSON is not an object")]
    NotAnObject,
    #[error("notebook has no `nbformat` field")]
    MissingVersion,
    #[error("notebook `nbformat` field is not an integer: {0}")]
    InvalidVersion(serde_json::Value),
    /// Only nbformat 4 is supported. Older notebooks are not upgraded yet.
    #[error("unsupported notebook format {major}.{minor}; only nbformat 4 is supported")]
    UnsupportedVersion { major: u64, minor: u64 },
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
