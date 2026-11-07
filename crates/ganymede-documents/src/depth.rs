//! How deeply a client's update nests its values, found without decoding it.
//!
//! yrs decodes an `Any` value (a JSON-like array or map) by recursing into
//! each nested value with no depth limit, and one level costs two bytes on
//! the wire. A small update nested deeply enough overflows the stack when
//! it is decoded, which aborts the process rather than panicking. So a
//! room scans each client update first, iteratively, following yrs 0.28's
//! v1 decoding (`Update::decode`, `ItemContent::decode`, `Any::decode`),
//! and refuses one that nests deeper than [`MAX_DEPTH`].
//!
//! Strings that yrs parses as JSON (embeds, formats, JSON content) need no
//! check: `serde_json` stops at 128 levels on its own.

use yrs::encoding::read::{Cursor, Error, Read};

/// The deepest nesting of `Any` values a client update may hold. A notebook
/// file can't nest deeper than `serde_json`'s 128 levels and still be read
/// back, so nothing legitimate comes near it.
pub const MAX_DEPTH: usize = 256;

/// Whether `update`, a v1 update, holds a value nested deeper than
/// [`MAX_DEPTH`]. A malformed update is not too deep: yrs refuses it when
/// it decodes it.
pub fn too_deep(update: &[u8]) -> bool {
    matches!(scan_update(&mut Cursor::new(update)), Err(Scan::TooDeep))
}

enum Scan {
    TooDeep,
    Malformed,
}

impl From<Error> for Scan {
    fn from(_: Error) -> Self {
        Scan::Malformed
    }
}

/// `Update::decode`: the blocks of each client. The delete set that follows
/// holds no values.
fn scan_update(c: &mut Cursor) -> Result<(), Scan> {
    let clients: u32 = c.read_var()?;
    for _ in 0..clients {
        let blocks: u32 = c.read_var()?;
        let _client: u64 = c.read_var()?;
        let _clock: u32 = c.read_var()?;
        for _ in 0..blocks {
            scan_block(c)?;
        }
    }
    Ok(())
}

const BLOCK_GC: u8 = 0;
const BLOCK_SKIP: u8 = 10;
const HAS_ORIGIN: u8 = 0b1000_0000;
const HAS_RIGHT_ORIGIN: u8 = 0b0100_0000;
const HAS_PARENT_SUB: u8 = 0b0010_0000;
const TYPE_REFS_XML_ELEMENT: u8 = 3;

/// `Update::decode_block`.
fn scan_block(c: &mut Cursor) -> Result<(), Scan> {
    match c.read_u8()? {
        BLOCK_SKIP | BLOCK_GC => {
            let _len: u32 = c.read_var()?;
        }
        info => {
            if info & HAS_ORIGIN != 0 {
                scan_id(c)?;
            }
            if info & HAS_RIGHT_ORIGIN != 0 {
                scan_id(c)?;
            }
            if info & (HAS_ORIGIN | HAS_RIGHT_ORIGIN) == 0 {
                let parent_info: u32 = c.read_var()?;
                if parent_info == 1 {
                    c.read_buf()?;
                } else {
                    scan_id(c)?;
                }
                if info & HAS_PARENT_SUB != 0 {
                    c.read_buf()?;
                }
            }
            scan_content(c, info & 0b1111)?;
        }
    }
    Ok(())
}

fn scan_id(c: &mut Cursor) -> Result<(), Scan> {
    let _client: u64 = c.read_var()?;
    let _clock: u32 = c.read_var()?;
    Ok(())
}

/// `ItemContent::decode`.
fn scan_content(c: &mut Cursor, content_ref: u8) -> Result<(), Scan> {
    match content_ref {
        // Deleted: a length.
        1 => {
            let _len: u32 = c.read_var()?;
        }
        // JSON: strings. yrs reads one more than the count it is given.
        2 => {
            let mut remaining = c.read_var::<u32>()? as i32;
            while remaining >= 0 {
                c.read_buf()?;
                remaining -= 1;
            }
        }
        // Binary, string, embed (JSON text).
        3..=5 => {
            c.read_buf()?;
        }
        // Format: a key and JSON text.
        6 => {
            c.read_buf()?;
            c.read_buf()?;
        }
        // A shared type; an XML element has a name.
        7 => {
            if c.read_u8()? == TYPE_REFS_XML_ELEMENT {
                c.read_buf()?;
            }
        }
        // Any values.
        8 => {
            let len: u32 = c.read_var()?;
            for _ in 0..len {
                scan_any(c)?;
            }
        }
        // Subdocument: a guid and its options.
        9 => {
            c.read_buf()?;
            scan_any(c)?;
        }
        _ => return Err(Scan::Malformed),
    }
    Ok(())
}

/// `Any::decode`, without recursing: `open` holds, for each array or map
/// being read, how many values are still to come and whether it is a map
/// (each of whose values follows a key).
fn scan_any(c: &mut Cursor) -> Result<(), Scan> {
    let mut open: Vec<(usize, bool)> = Vec::new();
    loop {
        let container = match c.read_u8()? {
            // undefined, null, false, true
            127 | 126 | 121 | 120 => None,
            // integer
            125 => {
                let _: i64 = c.read_var()?;
                None
            }
            // float32
            124 => {
                c.read_exact(4)?;
                None
            }
            // float64, bigint
            123 | 122 => {
                c.read_exact(8)?;
                None
            }
            // string, buffer
            119 | 116 => {
                c.read_buf()?;
                None
            }
            118 => Some((c.read_var::<usize>()?, true)),
            117 => Some((c.read_var::<usize>()?, false)),
            _ => return Err(Scan::Malformed),
        };
        if let Some((len, is_map)) = container {
            if open.len() >= MAX_DEPTH {
                return Err(Scan::TooDeep);
            }
            open.push((len, is_map));
        }
        // Find the next value to read, closing finished containers.
        loop {
            let Some((left, is_map)) = open.last_mut() else {
                return Ok(());
            };
            if *left == 0 {
                open.pop();
                continue;
            }
            *left -= 1;
            if *is_map {
                c.read_buf()?;
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Any, Array, Doc, Map, ReadTxn, Transact, WriteTxn};

    fn nested(depth: usize) -> Any {
        let mut v = Any::Null;
        for i in 0..depth {
            v = if i % 2 == 0 {
                Any::Array(vec![v].into())
            } else {
                Any::from(std::collections::HashMap::from([("k".to_string(), v)]))
            };
        }
        v
    }

    fn update_with(value: Any) -> Vec<u8> {
        let doc = Doc::new();
        let mut txn = doc.transact_mut();
        let map = txn.get_or_insert_map("m");
        map.insert(&mut txn, "deep", value);
        let arr = txn.get_or_insert_array("a");
        arr.push_back(&mut txn, Any::from("text"));
        arr.push_back(&mut txn, Any::Number(yrs::any::Number::Float(1.5)));
        arr.push_back(&mut txn, Any::Buffer(vec![1, 2, 3].into()));
        let text = txn.get_or_insert_text("t");
        yrs::Text::push(&text, &mut txn, "hello");
        map.remove(&mut txn, "gone");
        txn.encode_state_as_update_v1(&Default::default())
    }

    #[test]
    fn real_updates_are_not_too_deep() {
        for depth in [0, 1, 2, 50, MAX_DEPTH] {
            let update = update_with(nested(depth));
            assert!(
                matches!(scan_update(&mut Cursor::new(&update)), Ok(())),
                "depth {depth}"
            );
        }
    }

    #[test]
    fn deeper_than_the_limit_is_too_deep() {
        assert!(too_deep(&update_with(nested(MAX_DEPTH + 1))));
    }

    /// Far too deep for yrs to decode (it would overflow the stack), built
    /// by hand since yrs can't build or drop such a value either.
    #[test]
    fn a_huge_depth_is_found_without_recursing() {
        let depth = 1_000_000;
        let mut update = vec![1, 1, 7, 0]; // one client, one block, client 7, clock 0
        update.push(8); // item: Any content, no origins
        update.push(1); // parent named by a string
        update.extend([1, b'm']); // "m"
        update.push(1); // one Any value
        for _ in 0..depth {
            update.extend([117, 1]); // an array of one
        }
        update.push(126); // null
        update.push(0); // empty delete set
        assert!(too_deep(&update));
    }

    #[test]
    fn malformed_is_left_to_yrs() {
        assert!(!too_deep(&[]));
        assert!(!too_deep(&[1, 1, 7, 0, 8, 1, 1, b'm', 1, 117]));
    }
}
