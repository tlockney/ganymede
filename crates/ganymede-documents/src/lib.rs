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

pub use room::{ConnId, Effects, Room, RoomError, SaveStatus, Target};
pub use schema::SchemaError;
pub use yrs;

/// MurmurHash2 (32-bit) of `data`, as `jupyter-server-documents` hashes
/// a cell's UTF-8 source (seed 0) for its execute endpoint, so a request
/// names the source it saw and a changed cell is refused.
pub fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5BD1_E995;
    let mut h = seed ^ data.len() as u32;
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = u32::from_le_bytes(*c);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    if rest.len() == 3 {
        h ^= u32::from(rest[2]) << 16;
    }
    if rest.len() >= 2 {
        h ^= u32::from(rest[1]) << 8;
    }
    if !rest.is_empty() {
        h ^= u32::from(rest[0]);
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;
    h
}

#[cfg(test)]
mod tests {
    /// Values from `jupyter_server_documents.rooms.ynotebook_room._source_hash`.
    #[test]
    fn murmur2_matches_jupyter_server_documents() {
        for (s, h) in [
            ("", 0),
            ("a", 2456313694),
            ("ab", 446775395),
            ("abc", 324500635),
            ("abcd", 646393889),
            ("print('hello')\n", 1612567987),
            ("héllo wörld 🚀", 1432791399),
        ] {
            assert_eq!(super::murmur2(s.as_bytes(), 0), h, "{s:?}");
        }
    }
}
