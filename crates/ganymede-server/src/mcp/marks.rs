//! Read marks: what changed in a notebook since an agent last read it
//! (#313).
//!
//! A mark is the notebook's cells as a reader last saw them: each cell's
//! id and a fingerprint of its type and source, in order. Each MCP
//! connection has one mark per notebook, named by a random token that
//! `notebook_read` and `notebook_open` print. Reading moves the mark to
//! the notebook as read, and the connection's own edits move it too, so
//! the difference between a mark and the notebook is what others
//! changed: cells added, edited, deleted or moved. Outputs and metadata
//! are not compared.
//!
//! The token works from any connection to the same server, so a hook
//! outside the agent (such as a Claude Code `UserPromptSubmit` hook) can
//! ask on the agent's behalf; that read moves the agent's mark. Marks
//! live in memory: a restarted server knows none.

use ganymede_nbformat::{Cell, Notebook};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// How many marks a server keeps; the least recently used go first.
const LIMIT: usize = 4096;

/// A notebook's cells as a reader last saw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub path: String,
    pub cells: Vec<(String, u64)>,
    used: u64,
}

/// Every connection's marks, by token.
#[derive(Debug, Default)]
pub struct Marks {
    marks: HashMap<String, Mark>,
    clock: u64,
}

impl Marks {
    /// The mark named `token`, if the server still has it.
    pub fn get(&mut self, token: &str) -> Option<&mut Mark> {
        self.clock += 1;
        let clock = self.clock;
        let m = self.marks.get_mut(token)?;
        m.used = clock;
        Some(m)
    }

    /// Set `token`'s mark to `cells` of `path`.
    pub fn set(&mut self, token: &str, path: &str, cells: Vec<(String, u64)>) {
        self.clock += 1;
        if !self.marks.contains_key(token) && self.marks.len() >= LIMIT {
            if let Some(oldest) = self
                .marks
                .iter()
                .min_by_key(|(_, m)| m.used)
                .map(|(t, _)| t.clone())
            {
                self.marks.remove(&oldest);
            }
        }
        self.marks.insert(
            token.to_string(),
            Mark {
                path: path.to_string(),
                cells,
                used: self.clock,
            },
        );
    }
}

/// A new token.
pub fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

/// The cells of `nb` as a mark holds them. Cells without an id are left
/// out; a room gives every cell one.
pub fn snapshot(nb: &Notebook) -> Vec<(String, u64)> {
    nb.cells
        .iter()
        .filter_map(|c| c.id().map(|id| (id.to_string(), fingerprint(c))))
        .collect()
}

fn fingerprint(cell: &Cell) -> u64 {
    let (kind, source) = match cell {
        Cell::Code(c) => ("code", c.source.as_str()),
        Cell::Markdown(c) => ("markdown", c.source.as_str()),
        Cell::Raw(c) => ("raw", c.source.as_str()),
        _ => ("unknown", ""),
    };
    let mut h = DefaultHasher::new();
    kind.hash(&mut h);
    source.hash(&mut h);
    h.finish()
}

/// Take the reader's own change to cell `id` into `mark`, so it isn't
/// reported as someone else's: the cell's entry becomes what `now` has
/// (gone when `now` has no such cell), placed after the nearest cell
/// before it in `now` that the mark also has.
pub fn adopt(mark: &mut Vec<(String, u64)>, now: &[(String, u64)], id: &str) {
    mark.retain(|(i, _)| i != id);
    let Some(at) = now.iter().position(|(i, _)| i == id) else {
        return;
    };
    let index = now[..at]
        .iter()
        .rev()
        .find_map(|(prev, _)| mark.iter().position(|(i, _)| i == prev))
        .map_or(0, |p| p + 1);
    mark.insert(index, now[at].clone());
}

/// What changed between a mark and the notebook now, as cell ids.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub edited: Vec<String>,
    pub moved: Vec<String>,
    pub deleted: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.edited.is_empty()
            && self.moved.is_empty()
            && self.deleted.is_empty()
    }
}

/// The changes from `old` to `new`. A cell is moved when it isn't part of
/// the longest run of cells the two keep in the same order; a cell both
/// moved and edited is reported in both.
pub fn diff(old: &[(String, u64)], new: &[(String, u64)]) -> Changes {
    let old_fp: HashMap<&str, u64> = old.iter().map(|(i, f)| (i.as_str(), *f)).collect();
    let new_ids: HashSet<&str> = new.iter().map(|(i, _)| i.as_str()).collect();
    let mut c = Changes::default();
    for (id, fp) in new {
        match old_fp.get(id.as_str()) {
            None => c.added.push(id.clone()),
            Some(f) if f != fp => c.edited.push(id.clone()),
            Some(_) => {}
        }
    }
    c.deleted = old
        .iter()
        .filter(|(i, _)| !new_ids.contains(i.as_str()))
        .map(|(i, _)| i.clone())
        .collect();
    // The cells both have, in each one's order.
    let a: Vec<&str> = old
        .iter()
        .map(|(i, _)| i.as_str())
        .filter(|i| new_ids.contains(i))
        .collect();
    let b: Vec<&str> = new
        .iter()
        .map(|(i, _)| i.as_str())
        .filter(|i| old_fp.contains_key(i))
        .collect();
    let kept = lcs(&a, &b);
    c.moved = b
        .iter()
        .filter(|i| !kept.contains(*i))
        .map(|i| i.to_string())
        .collect();
    c
}

/// The longest common subsequence of `a` and `b`, which hold the same
/// ids, as a set.
fn lcs<'a>(a: &[&'a str], b: &[&'a str]) -> HashSet<&'a str> {
    // As the longest increasing run of `a`'s positions taken in `b`'s
    // order (patience sorting), in O(n log n).
    let pos: HashMap<&str, usize> = a.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let seq: Vec<usize> = b.iter().map(|id| pos[id]).collect();
    // tails[k]: index into seq of the smallest tail of a run of k + 1.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; seq.len()];
    for (i, &v) in seq.iter().enumerate() {
        let k = tails.partition_point(|&t| seq[t] < v);
        prev[i] = k.checked_sub(1).map(|k| tails[k]);
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut kept = HashSet::new();
    let mut at = tails.last().copied();
    while let Some(i) = at {
        kept.insert(b[i]);
        at = prev[i];
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(spec: &[(&str, u64)]) -> Vec<(String, u64)> {
        spec.iter().map(|(i, f)| (i.to_string(), *f)).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn diff_finds_each_kind_of_change() {
        let old = cells(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)]);
        assert!(diff(&old, &old).is_empty());
        // b edited, c deleted, e added, d moved to the top.
        let new = cells(&[("d", 4), ("a", 1), ("b", 9), ("e", 5)]);
        let c = diff(&old, &new);
        assert_eq!(c.added, ids(&["e"]));
        assert_eq!(c.edited, ids(&["b"]));
        assert_eq!(c.deleted, ids(&["c"]));
        assert_eq!(c.moved, ids(&["d"]));
    }

    #[test]
    fn diff_reports_the_fewest_moves() {
        let old = cells(&[("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0)]);
        // e moved from the end to second place.
        let new = cells(&[("a", 0), ("e", 0), ("b", 0), ("c", 0), ("d", 0)]);
        assert_eq!(diff(&old, &new).moved, ids(&["e"]));
        // Reversed: all but one moved.
        let rev: Vec<_> = old.iter().rev().cloned().collect();
        assert_eq!(diff(&old, &rev).moved.len(), 4);
    }

    #[test]
    fn adopt_takes_the_readers_own_changes() {
        let mut mark = cells(&[("a", 1), ("b", 2)]);
        // The reader added x after a; someone else added y at the end and
        // edited b.
        let now = cells(&[("a", 1), ("x", 7), ("b", 9), ("y", 8)]);
        adopt(&mut mark, &now, "x");
        assert_eq!(mark, cells(&[("a", 1), ("x", 7), ("b", 2)]));
        let c = diff(&mark, &now);
        assert_eq!(c.added, ids(&["y"]));
        assert_eq!(c.edited, ids(&["b"]));
        assert!(c.moved.is_empty() && c.deleted.is_empty());
        // The reader deleted a, and moved b to the top.
        let now = cells(&[("b", 2), ("x", 7)]);
        adopt(&mut mark, &now, "a");
        adopt(&mut mark, &now, "b");
        assert_eq!(mark, cells(&[("b", 2), ("x", 7)]));
        assert!(diff(&mark, &now).is_empty());
    }

    #[test]
    fn marks_forget_the_least_recently_used() {
        let mut m = Marks::default();
        for n in 0..LIMIT {
            m.set(&n.to_string(), "p", Vec::new());
        }
        assert!(m.get("0").is_some());
        m.set("new", "p", Vec::new());
        assert!(m.get("0").is_some(), "used last, kept");
        assert!(m.get("1").is_none(), "least recently used, dropped");
        assert_eq!(m.marks.len(), LIMIT);
    }
}
