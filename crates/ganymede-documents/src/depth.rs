//! How deeply a client's update nests, so a room can refuse one that
//! would overflow the stack. Nothing here applies the update or recurses.
//!
//! Two kinds of nesting:
//!
//! - **Values.** yrs decodes an `Any` value (a JSON-like array or map) by
//!   recursing into each nested value with no depth limit, and one level
//!   costs two bytes on the wire. [`check`] scans the update, following
//!   yrs 0.28's v1 decoding (`Update::decode`, `ItemContent::decode`,
//!   `Any::decode`), and finds values nested deeper than [`MAX_DEPTH`].
//! - **Shared types.** Maps, arrays and texts nested in one another, each
//!   level its own item, possibly over many updates. yrs deletes them
//!   recursively, during an apply too (a key overwritten in the same
//!   update deletes the tree under it), so the depth has to be known
//!   before the update is applied. [`check`] records each item's place and
//!   parent, and [`Checked::type_depths`] works out how deep the update's
//!   new shared types would sit, asking the room how deep the items they
//!   hang on in the document are ([`shared_depths`] measures that).
//!
//! Strings that yrs parses as JSON (embeds, formats, JSON content) need no
//! check: `serde_json` stops at 128 levels on its own. A yrs upgrade that
//! changes the v1 format has to update this module.

use std::collections::HashMap;
use yrs::branch::{Branch, BranchID, BranchPtr};
use yrs::encoding::read::{Cursor, Error, Read};
use yrs::types::text::{Diff, YChange};
use yrs::{ClientID, Map, MapRef, Out, ReadTxn, StateVector, Text, TextRef};

/// The deepest nesting of `Any` values a client update may hold.
pub const MAX_DEPTH: usize = 100;

/// The deepest nesting of shared types a document may reach, counting a
/// root as 1. The notebook schema goes 5 deep (cells, a cell, its outputs,
/// an output, its text). With [`MAX_DEPTH`] on top, and the notebook's own
/// levels, a document stays inside the 128 levels `serde_json` reads back.
pub const MAX_SHARED_DEPTH: usize = 16;

/// How deep the server's own edits nest shared types: the notebook schema
/// (cells, a cell, its outputs, an output, its text). A room counts its
/// document as at least this deep.
pub const SCHEMA_DEPTH: usize = 5;

/// What [`check`] found in an update.
#[derive(Debug, Default)]
pub struct Checked {
    /// A value nests deeper than [`MAX_DEPTH`].
    pub too_deep: bool,
    /// The scan couldn't follow the update. yrs may still read it (a clock
    /// that wraps, say), and then nothing here was checked, so the room
    /// refuses it.
    pub malformed: bool,
    /// The update's items, in order, for [`Checked::type_depths`].
    items: Vec<ItemRec>,
}

/// Where an item hangs, as the update says.
#[derive(Debug, Clone, Copy)]
enum ParentRef {
    /// A root type, named.
    Root,
    /// The shared type that the item with this id holds.
    Type(u64, u32),
    /// Beside its origins (left, right), whose parent it takes, as yrs's
    /// `Update::repair` picks it: the left one's when that is an item with
    /// a known parent, else the right one's.
    Beside(Option<(u64, u32)>, Option<(u64, u32)>),
    /// Not an item: garbage-collected or skipped clocks.
    None,
}

#[derive(Debug, Clone, Copy)]
struct ItemRec {
    client: u64,
    clock: u32,
    len: u32,
    parent: ParentRef,
    is_type: bool,
    /// A skip block: a gap the update doesn't fill.
    skip: bool,
}

/// Why [`Checked::type_depths`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// The update hangs items on ones the document doesn't have yet. yrs
    /// would hold them until those arrive, and they would land at a depth
    /// nobody checked.
    Missing,
}

/// An item in the document that an update's item hangs on, for
/// [`Checked::type_depths`] to ask the room about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRef {
    /// The item holds the shared type that is the parent.
    Type(u64, u32),
    /// The item is a sibling: same parent.
    Beside(u64, u32),
}

/// The depths of the shared types an update creates.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TypeDepths {
    /// Each new shared type's item id (client, clock) and depth, a root
    /// counting as 1.
    pub types: Vec<(u64, u32, usize)>,
    /// The deepest of them; 0 for none.
    pub deepest: usize,
    /// Some depth stood in for one the room couldn't tell (the bound
    /// given): the result can overstate.
    pub estimated: bool,
}

impl Checked {
    /// Whether the update creates shared types, so it can deepen the
    /// document's nesting of them.
    pub fn adds_types(&self) -> bool {
        self.items.iter().any(|i| i.is_type)
    }

    /// How deep each shared type the update creates would sit. `sv` is the
    /// document's state vector, to tell which referenced items it has.
    /// For an item hung on one in the document, `live` gives that parent's
    /// depth, or `None` when it can't tell; then `bound`, how deep the
    /// document goes at most, stands in.
    pub fn type_depths(
        &self,
        sv: &StateVector,
        bound: usize,
        mut live: impl FnMut(LiveRef) -> Option<usize>,
    ) -> Result<TypeDepths, Unresolved> {
        let mut by_client: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, item) in self.items.iter().enumerate() {
            by_client.entry(item.client).or_default().push(i);
        }
        for idxs in by_client.values_mut() {
            idxs.sort_by_key(|&i| self.items[i].clock);
        }
        // yrs holds a run that starts past what the document has.
        for (client, idxs) in &by_client {
            if self.items[idxs[0]].clock > sv.get(&ClientID::new(*client)) {
                return Err(Unresolved::Missing);
            }
        }
        let known = |client: u64, clock: u32| clock < sv.get(&ClientID::new(client));
        // An id in the update, unless the document has it already: yrs
        // skips the update's copy of an item it has and uses its own, so a
        // re-sent copy must not speak for it.
        let find = |client: u64, clock: u32| -> Option<usize> {
            if known(client, clock) {
                return None;
            }
            let idxs = by_client.get(&client)?;
            let pos = idxs.partition_point(|&i| self.items[i].clock <= clock);
            let i = *idxs.get(pos.checked_sub(1)?)?;
            let it = &self.items[i];
            (clock < it.clock + it.len).then_some(i)
        };
        let bound = bound.max(1);
        let mut out = TypeDepths::default();

        // Every id an item hangs on must be in the update or the document:
        // yrs holds an item that names one it doesn't have.
        for item in &self.items {
            let ids = match item.parent {
                ParentRef::Type(c, k) => [Some((c, k)), None],
                ParentRef::Beside(l, r) => [l, r],
                ParentRef::Root | ParentRef::None => [None, None],
            };
            for (c, k) in ids.into_iter().flatten() {
                if find(c, k).is_none() && !known(c, k) {
                    return Err(Unresolved::Missing);
                }
            }
        }

        // Each item's parent depth, `Gone` when yrs won't attach it (its
        // parent is not a shared type, or neither origin gives one), so
        // nothing hangs below it. Resolved without recursing: what an item
        // depends on in the update is resolved first, with a stack, and a
        // cycle (malformed) is unresolved.
        #[derive(Clone, Copy, PartialEq)]
        enum Res {
            Depth(usize),
            Gone,
        }
        #[derive(Clone, Copy)]
        enum State {
            Todo,
            Busy,
            Done(Res),
        }
        let mut state = vec![State::Todo; self.items.len()];
        // An item a neighbour or child depends on: `None` for one yrs
        // wouldn't find as an item (garbage-collected or skipped clocks).
        let item_at = |c: u64, k: u32| {
            find(c, k).filter(|&j| {
                !self.items[j].skip && !matches!(self.items[j].parent, ParentRef::None)
            })
        };
        for start in 0..self.items.len() {
            // A type the document already has is not new; yrs skips it.
            let it = &self.items[start];
            if !it.is_type || known(it.client, it.clock) {
                continue;
            }
            let mut stack = vec![start];
            'resolve: while let Some(&i) = stack.last() {
                if let State::Done(_) = state[i] {
                    stack.pop();
                    continue;
                }
                state[i] = State::Busy;
                // What `i` depends on in the update, resolved first.
                let deps: [Option<usize>; 2] = match self.items[i].parent {
                    ParentRef::Type(c, k) => [item_at(c, k), None],
                    ParentRef::Beside(l, r) => [
                        l.and_then(|(c, k)| item_at(c, k)),
                        r.and_then(|(c, k)| item_at(c, k)),
                    ],
                    ParentRef::Root | ParentRef::None => [None, None],
                };
                for j in deps.into_iter().flatten() {
                    match state[j] {
                        State::Done(_) => {}
                        State::Busy => return Err(Unresolved::Missing),
                        State::Todo => {
                            stack.push(j);
                            continue 'resolve;
                        }
                    }
                }
                let done = |j: usize| match state[j] {
                    State::Done(r) => r,
                    _ => unreachable!("resolved above"),
                };
                let mut estimate = || {
                    out.estimated = true;
                    Res::Depth(bound)
                };
                let res = match self.items[i].parent {
                    ParentRef::Root => Res::Depth(1),
                    ParentRef::None => Res::Gone,
                    ParentRef::Type(c, k) => match find(c, k) {
                        // In the update: a shared type gives its depth;
                        // anything else leaves `i` unattached.
                        Some(j) if self.items[j].is_type => match done(j) {
                            Res::Depth(d) => Res::Depth(d + 1),
                            Res::Gone => Res::Gone,
                        },
                        Some(_) => Res::Gone,
                        // In the document.
                        None => match live(LiveRef::Type(c, k)) {
                            Some(d) => Res::Depth(d),
                            None => estimate(),
                        },
                    },
                    ParentRef::Beside(l, r) => {
                        // An origin's parent depth: `None` when yrs would
                        // pass it over (not an item, or one not attached).
                        let mut side = |o: Option<(u64, u32)>| -> Option<Res> {
                            let (c, k) = o?;
                            match find(c, k) {
                                Some(_) => match item_at(c, k).map(done) {
                                    Some(Res::Depth(d)) => Some(Res::Depth(d)),
                                    _ => None,
                                },
                                // In the document: a sibling that holds a
                                // shared type is one below the parent.
                                // Any other might be anywhere, or be
                                // garbage-collected (then yrs uses the
                                // other side); the deepest stands in.
                                None => match live(LiveRef::Beside(c, k)) {
                                    Some(d) => Some(Res::Depth(d)),
                                    None => Some(estimate()),
                                },
                            }
                        };
                        side(l).or_else(|| side(r)).unwrap_or(Res::Gone)
                    }
                };
                state[i] = State::Done(res);
                let item = &self.items[i];
                if let (true, Res::Depth(d)) = (item.is_type, res) {
                    let depth = d + 1;
                    out.types.push((item.client, item.clock, depth));
                    out.deepest = out.deepest.max(depth);
                }
                stack.pop();
            }
        }
        Ok(out)
    }
}

/// Scan `update`, a v1 update. One the scan can't follow is `malformed`,
/// and refused: a guard that let it through would leave yrs to decode it
/// unchecked.
pub fn check(update: &[u8]) -> Checked {
    let mut checked = Checked::default();
    match scan_update(&mut Cursor::new(update), &mut checked.items) {
        Ok(()) => checked,
        Err(Scan::TooDeep) => Checked {
            too_deep: true,
            ..Checked::default()
        },
        Err(Scan::Malformed) => Checked {
            malformed: true,
            ..Checked::default()
        },
    }
}

/// The depth of each nested shared type in the document, by the id
/// (client, clock) of the item holding it, and the deepest, counting a
/// root as 1 (0 for none). Walks every root's maps and sequences (embedded
/// types included) with a stack of its own, and stops descending past
/// [`MAX_SHARED_DEPTH`].
pub fn shared_depths<T: ReadTxn>(txn: &T) -> (HashMap<(u64, u32), usize>, usize) {
    let mut stack: Vec<(BranchPtr, usize)> = txn
        .root_refs()
        .filter_map(|(_, out)| branch_of(&out))
        .map(|b| (b, 1))
        .collect();
    let mut depths = HashMap::new();
    let mut deepest = 0;
    while let Some((branch, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        if let BranchID::Nested(id) = branch.id() {
            depths.insert((id.client.get(), id.clock), depth);
        }
        if depth > MAX_SHARED_DEPTH {
            continue;
        }
        // Any branch can hold keyed entries and a sequence, whatever its
        // type; read both through the generic views.
        let map = MapRef::from(branch);
        for (_, value) in map.iter(txn) {
            if let Some(child) = branch_of(&value) {
                stack.push((child, depth + 1));
            }
        }
        let text = TextRef::from(branch);
        let diffs: Vec<Diff<YChange>> = text.diff(txn, YChange::identity);
        for diff in diffs {
            if let Some(child) = branch_of(&diff.insert) {
                stack.push((child, depth + 1));
            }
        }
    }
    (depths, deepest)
}

fn branch_of(out: &Out) -> Option<BranchPtr> {
    Some(match out {
        Out::YText(t) => BranchPtr::from(AsRef::<Branch>::as_ref(t)),
        Out::YArray(a) => BranchPtr::from(AsRef::<Branch>::as_ref(a)),
        Out::YMap(m) => BranchPtr::from(AsRef::<Branch>::as_ref(m)),
        Out::YXmlElement(x) => BranchPtr::from(AsRef::<Branch>::as_ref(x)),
        Out::YXmlFragment(x) => BranchPtr::from(AsRef::<Branch>::as_ref(x)),
        Out::YXmlText(x) => BranchPtr::from(AsRef::<Branch>::as_ref(x)),
        Out::UndefinedRef(b) => *b,
        _ => return None,
    })
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
/// holds no values and adds no items.
fn scan_update(c: &mut Cursor, items: &mut Vec<ItemRec>) -> Result<(), Scan> {
    let clients: u32 = c.read_var()?;
    for _ in 0..clients {
        let blocks: u32 = c.read_var()?;
        let client: u64 = c.read_var()?;
        let mut clock: u32 = c.read_var()?;
        for _ in 0..blocks {
            let (len, parent, is_type, skip) = scan_block(c)?;
            // yrs drops an empty item without advancing the clock.
            if len > 0 {
                items.push(ItemRec {
                    client,
                    clock,
                    len,
                    parent,
                    is_type,
                    skip,
                });
                clock = clock.checked_add(len).ok_or(Scan::Malformed)?;
            }
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

/// `Update::decode_block`: the block's length in clocks, its parent, and
/// whether it holds a shared type or is a skip.
fn scan_block(c: &mut Cursor) -> Result<(u32, ParentRef, bool, bool), Scan> {
    Ok(match c.read_u8()? {
        BLOCK_SKIP => (c.read_var()?, ParentRef::None, false, true),
        BLOCK_GC => (c.read_var()?, ParentRef::None, false, false),
        info => {
            let origin = (info & HAS_ORIGIN != 0).then(|| scan_id(c)).transpose()?;
            let right = (info & HAS_RIGHT_ORIGIN != 0)
                .then(|| scan_id(c))
                .transpose()?;
            let parent = match (origin, right) {
                (None, None) => {
                    let parent_info: u32 = c.read_var()?;
                    let parent = if parent_info == 1 {
                        c.read_buf()?;
                        ParentRef::Root
                    } else {
                        let (client, clock) = scan_id(c)?;
                        ParentRef::Type(client, clock)
                    };
                    if info & HAS_PARENT_SUB != 0 {
                        c.read_buf()?;
                    }
                    parent
                }
                (left, right) => ParentRef::Beside(left, right),
            };
            let (len, is_type) = scan_content(c, info & 0b1111)?;
            (len, parent, is_type, false)
        }
    })
}

fn scan_id(c: &mut Cursor) -> Result<(u64, u32), Scan> {
    let client: u64 = c.read_var()?;
    let clock: u32 = c.read_var()?;
    Ok((client, clock))
}

/// `ItemContent::decode`: the content's length in clocks (UTF-16 units
/// for a string, as yrs counts them) and whether it is a shared type.
fn scan_content(c: &mut Cursor, content_ref: u8) -> Result<(u32, bool), Scan> {
    Ok(match content_ref {
        // Deleted: a length.
        1 => (c.read_var()?, false),
        // JSON: strings. yrs reads one more than the count it is given.
        2 => {
            let mut remaining = c.read_var::<u32>()? as i64;
            let mut len = 0u32;
            while remaining >= 0 {
                c.read_buf()?;
                remaining -= 1;
                len += 1;
            }
            (len, false)
        }
        // Binary, embed (JSON text): one each.
        3 | 5 => {
            c.read_buf()?;
            (1, false)
        }
        // String.
        4 => {
            let s = std::str::from_utf8(c.read_buf()?).map_err(|_| Scan::Malformed)?;
            let len = s.encode_utf16().count();
            (u32::try_from(len).map_err(|_| Scan::Malformed)?, false)
        }
        // Format: a key and JSON text.
        6 => {
            c.read_buf()?;
            c.read_buf()?;
            (1, false)
        }
        // A shared type; an XML element has a name.
        7 => {
            if c.read_u8()? == TYPE_REFS_XML_ELEMENT {
                c.read_buf()?;
            }
            (1, true)
        }
        // Any values.
        8 => {
            let len: u32 = c.read_var()?;
            for _ in 0..len {
                scan_any(c)?;
            }
            (len, false)
        }
        // Subdocument: a guid and its options.
        9 => {
            c.read_buf()?;
            scan_any(c)?;
            (1, false)
        }
        _ => return Err(Scan::Malformed),
    })
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

    fn too_deep(update: &[u8]) -> bool {
        check(update).too_deep
    }
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
            let checked = check(&update);
            assert!(!checked.too_deep, "depth {depth}");
            assert!(!checked.adds_types(), "depth {depth}");
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
