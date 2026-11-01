//! Document rooms: a notebook held live as a [`yrs`] (Yjs) document, which
//! clients edit together over the y-websocket protocol.
//!
//! - [`schema`] maps a [`ganymede_nbformat::Notebook`] to and from the Y
//!   document schema JupyterLab's collaboration client
//!   (`jupyter-collaboration`, `@jupyter/ydoc`) expects.
//! - [`room::Room`] is one document and its connected clients, as a state
//!   machine with no I/O: the host feeds it frames and carries out the
//!   [`room::Effects`] it returns. `ganymede-server` hosts rooms behind
//!   `/api/collaboration`; the apps can host them in-process through
//!   `ganymede-ffi`.

pub mod room;
pub mod schema;

pub use room::{ConnId, Effects, Room, RoomError, SaveStatus, Target};
pub use schema::SchemaError;
pub use yrs;
