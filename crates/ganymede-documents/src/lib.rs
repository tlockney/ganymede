//! Document rooms: a notebook held live as a [`yrs`] (Yjs) document, which
//! clients edit together over the y-websocket protocol.
//!
//! - [`schema`] maps a [`ganymede_nbformat::Notebook`] to and from the Y
//!   document schema JupyterLab's collaboration client
//!   (`jupyter-collaboration`, `@jupyter/ydoc`) expects.
//! - [`merge`] merges a file changed on disk into a room's unsaved edits.
//! - [`room::Room`] is one document and its connected clients, as a state
//!   machine with no I/O: the host feeds it frames and carries out the
//!   [`room::Effects`] it returns. `ganymede-server` hosts rooms behind
//!   `/api/collaboration`; the apps can host them in-process through
//!   `ganymede-ffi`.

pub mod depth;
pub mod merge;
pub mod room;
pub mod schema;

pub use room::{clean_name, Author, ConnId, Effects, Room, RoomError, SaveStatus, Target};
pub use schema::SchemaError;
pub use yrs;

/// MurmurHash2 (32-bit), as `jupyter-server-documents` hashes a cell's
/// UTF-8 source (seed 0) for its execute endpoint, so a request names the
/// source it saw and a changed cell is refused. Lives in
/// [`ganymede_nbformat`], with [`ganymede_nbformat::source_hash`].
pub use ganymede_nbformat::murmur2;
