//! Read marks: what changed in a notebook since an agent last read it
//! (#313).
//!
//! A mark is the notebook's cells as a reader last saw them: each cell's
//! id and a fingerprint of its type and source, in order. Each MCP
//! connection has one mark per notebook, named by a random token that
//! `notebook_read` and `notebook_open` print. Reading moves the mark to
//! the notebook as read, and the connection's own edits move it too, so
//! the difference between a mark and the notebook is what others
//! changed: cells added, edited, deleted or moved. A mark also holds
//! each code cell's execution count, so a cell someone else ran is
//! reported as `ran`; the connection's own runs move that too. Outputs
//! and metadata are not compared.
//!
//! The token works from any connection to the same server, so a hook
//! outside the agent (such as a Claude Code `UserPromptSubmit` hook) can
//! ask on the agent's behalf; that read moves the agent's mark. Marks
//! live in memory: a restarted server knows none.

use ganymede_documents::{Author, Room};
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
    /// Each run code cell's execution count (see [`runs`]).
    pub runs: HashMap<String, u64>,
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
    pub fn set(
        &mut self,
        token: &str,
        path: &str,
        cells: Vec<(String, u64)>,
        runs: HashMap<String, u64>,
    ) {
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
                runs,
                used: self.clock,
            },
        );
    }
}

/// A new token.
pub fn token() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

/// Whether `cell` asks the agent something: a Markdown cell whose text
/// starts with `@agent`, or `@` and the client's name (`@claude-code`) or
/// its first part (`@claude`), ignoring case, followed by a space, a
/// punctuation mark or nothing, and not yet answered (`cell_add(answers)`
/// sets `metadata.ganymede.request` with `done` and this cell's `id`). Who wrote it is checked by the
/// caller: only a person's cell counts.
pub fn is_request(cell: &Cell, client: &str) -> bool {
    let Cell::Markdown(m) = cell else {
        return false;
    };
    // Done only for this cell: a copy of an answered request (paste,
    // duplicate, split) keeps the metadata under a new id, and a question
    // written in it is a new request.
    let done = m
        .metadata
        .get("ganymede")
        .and_then(|g| g.get("request"))
        .filter(|r| r.get("done").is_some())
        .and_then(|r| r.get("id"))
        .and_then(|i| i.as_str());
    if done.is_some() && done == m.id.as_deref() {
        return false;
    }
    let text = m.source.trim_start();
    let Some(rest) = text.strip_prefix('@') else {
        return false;
    };
    let word: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let word = word.to_lowercase();
    let client = client.to_lowercase();
    let short = client.split(['-', '_', ' ']).next().unwrap_or_default();
    !word.is_empty() && (word == "agent" || word == client || word == short)
}

/// The cells of `nb` as a mark holds them. Cells without an id are left
/// out; a room gives every cell one.
pub fn snapshot(nb: &Notebook) -> Vec<(String, u64)> {
    nb.cells
        .iter()
        .filter_map(|c| c.id().map(|id| (id.to_string(), fingerprint(c))))
        .collect()
}

/// Each code cell's last run in `nb`, by its execution count: every run
/// takes a new one. Outputs aren't compared, so a display update from
/// another cell isn't a run of this one. A cell that hasn't run, or whose
/// outputs were cleared with its count, has none, so clearing isn't
/// reported as a run.
pub fn runs(nb: &Notebook) -> HashMap<String, u64> {
    nb.cells
        .iter()
        .filter_map(|c| match c {
            Cell::Code(code) => Some((code.id.clone()?, code.execution_count?)),
            _ => None,
        })
        .collect()
}

/// Take the reader's own runs of cells `ids` into `mark`, as `now` has
/// them.
pub fn adopt_runs(mark: &mut HashMap<String, u64>, now: &HashMap<String, u64>, ids: &[String]) {
    for id in ids {
        match now.get(id) {
            Some(fp) => mark.insert(id.clone(), *fp),
            None => mark.remove(id),
        };
    }
}

/// The cells of `cells` (in order) that ran since `old`, by `new`:
/// those the mark already had, whose run differs. A cell added since the
/// mark is reported as added, not ran.
pub fn ran(
    added: &[String],
    cells: &[(String, u64)],
    old: &HashMap<String, u64>,
    new: &HashMap<String, u64>,
) -> Vec<String> {
    let added: HashSet<&String> = added.iter().collect();
    cells
        .iter()
        .map(|(id, _)| id)
        .filter(|id| !added.contains(id))
        .filter(|id| new.get(*id).is_some_and(|fp| old.get(*id) != Some(fp)))
        .cloned()
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

/// What the reader's own edit changed about a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    /// The cell came or went (`cell_add`, `cell_run`, `cell_delete`).
    All,
    /// Its source (`cell_edit`).
    Source,
    /// Its place (`cell_move`).
    Place,
}

/// Take the reader's own change to cell `id` into `mark`, so it isn't
/// reported as someone else's. Only the `aspect` the reader changed is
/// taken from `now`; the rest of the mark's entry stays, so someone
/// else's change to the other aspect is still reported. A cell `now`
/// lacks leaves the mark; a cell the mark lacks comes in whole, after
/// the nearest cell before it in `now` that the mark also has.
pub fn adopt(mark: &mut Vec<(String, u64)>, now: &[(String, u64)], id: &str, aspect: Aspect) {
    let Some(at) = now.iter().position(|(i, _)| i == id) else {
        mark.retain(|(i, _)| i != id);
        return;
    };
    let had = mark.iter().position(|(i, _)| i == id);
    if let (Aspect::Source, Some(h)) = (aspect, had) {
        mark[h].1 = now[at].1;
        return;
    }
    let fp = match (aspect, had) {
        (Aspect::Place, Some(h)) => mark[h].1,
        _ => now[at].1,
    };
    mark.retain(|(i, _)| i != id);
    let index = now[..at]
        .iter()
        .rev()
        .find_map(|(prev, _)| mark.iter().position(|(i, _)| i == prev))
        .map_or(0, |p| p + 1);
    mark.insert(index, (id.to_string(), fp));
}

/// Whether cell `id` changed since the reader's `mark`: it is in `now`,
/// and the mark lacks it or holds another source or type for it. A cell
/// `now` lacks is no change of someone else's to lose.
pub fn unseen(mark: &[(String, u64)], now: &[(String, u64)], id: &str) -> bool {
    let Some((_, fp)) = now.iter().find(|(i, _)| i == id) else {
        return false;
    };
    !mark.iter().any(|(i, f)| i == id && f == fp)
}

/// What changed between a mark and the notebook now, as cell ids.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub edited: Vec<String>,
    pub moved: Vec<String>,
    pub deleted: Vec<String>,
    /// Code cells someone else ran (see [`ran`]).
    pub ran: Vec<String>,
    /// Who last changed each added or edited cell, where the room knows
    /// (see [`Changes::credit`]).
    pub by: HashMap<String, Author>,
    /// The added or edited cells that are requests to the agent (see
    /// [`is_request`]).
    pub requests: Vec<String>,
}

impl Changes {
    /// Fill in [`Changes::by`] and [`Changes::requests`] from the room
    /// that holds `nb`, for a reader whose client is named `client`.
    pub fn credit(&mut self, room: &Room, nb: &Notebook, client: &str) {
        for id in self.added.iter().chain(&self.edited) {
            if let Some(a) = room.author(id) {
                self.by.insert(id.clone(), a.clone());
            }
        }
        self.requests = nb
            .cells
            .iter()
            .filter_map(|c| {
                let id = c.id()?;
                let by = self.by.get(id)?;
                (by.person && is_request(c, client)).then(|| id.to_string())
            })
            .collect();
    }

    /// The distinct names behind the added and edited cells, in order.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for id in self.added.iter().chain(&self.edited) {
            if let Some(a) = self.by.get(id) {
                if !names.contains(&a.name.as_str()) {
                    names.push(&a.name);
                }
            }
        }
        names
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.edited.is_empty()
            && self.moved.is_empty()
            && self.deleted.is_empty()
            && self.ran.is_empty()
    }
}

/// What changed since mark `m`, given the notebook's cells `now` and
/// their `runs`.
pub fn since(m: &Mark, now: &[(String, u64)], runs: &HashMap<String, u64>) -> Changes {
    let mut c = diff(&m.cells, now);
    c.ran = ran(&c.added, now, &m.runs, runs);
    c
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
    fn ran_finds_cells_others_ran() {
        let cells = cells(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)]);
        let old: HashMap<String, u64> = [("a".into(), 10), ("b".into(), 20)].into();
        // a ran again, b is as it was, c ran for the first time, d was
        // added since the mark (reported as added), and a cleared cell
        // (not in `new`) isn't a run.
        let new: HashMap<String, u64> = [
            ("a".into(), 11),
            ("b".into(), 20),
            ("c".into(), 30),
            ("d".into(), 40),
        ]
        .into();
        assert_eq!(ran(&ids(&["d"]), &cells, &old, &new), ids(&["a", "c"]));
        let mut mine = old.clone();
        adopt_runs(&mut mine, &new, &ids(&["a", "c"]));
        assert!(ran(&ids(&["d"]), &cells, &mine, &new).is_empty());
        adopt_runs(&mut mine, &HashMap::new(), &ids(&["a"]));
        assert!(!mine.contains_key("a"));
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
        adopt(&mut mark, &now, "x", Aspect::All);
        assert_eq!(mark, cells(&[("a", 1), ("x", 7), ("b", 2)]));
        let c = diff(&mark, &now);
        assert_eq!(c.added, ids(&["y"]));
        assert_eq!(c.edited, ids(&["b"]));
        assert!(c.moved.is_empty() && c.deleted.is_empty());
        // The reader deleted a, and moved b to the top.
        let now = cells(&[("b", 2), ("x", 7)]);
        adopt(&mut mark, &now, "a", Aspect::All);
        adopt(&mut mark, &now, "b", Aspect::Place);
        assert_eq!(mark, cells(&[("b", 2), ("x", 7)]));
        assert!(diff(&mark, &now).is_empty());
        // Someone else edited x, then the reader moved it: still an edit.
        let now = cells(&[("x", 9), ("b", 2)]);
        adopt(&mut mark, &now, "x", Aspect::Place);
        let c = diff(&mark, &now);
        assert_eq!(c.edited, ids(&["x"]));
        assert!(c.moved.is_empty());
        // Someone else moved b to the end; the reader edited x: the move
        // is still reported, and the edit isn't.
        let mut mark = cells(&[("a", 1), ("b", 2), ("x", 3)]);
        let now = cells(&[("a", 1), ("x", 4), ("b", 2)]);
        adopt(&mut mark, &now, "x", Aspect::Source);
        let c = diff(&mark, &now);
        assert!(c.edited.is_empty());
        assert_eq!(c.moved.len(), 1);
    }

    #[test]
    fn unseen_is_a_cell_changed_or_added_since_the_mark() {
        let mark = cells(&[("a", 1), ("b", 2)]);
        let now = cells(&[("a", 1), ("b", 9), ("c", 3)]);
        assert!(!unseen(&mark, &now, "a"), "as read");
        assert!(unseen(&mark, &now, "b"), "edited since");
        assert!(unseen(&mark, &now, "c"), "added since");
        assert!(!unseen(&mark, &now, "gone"), "not there now");
    }

    #[test]
    fn requests_name_the_agent_first() {
        let cell = |c: serde_json::Value| {
            let nb = serde_json::json!({"cells": [c], "metadata": {},
                "nbformat": 4, "nbformat_minor": 5});
            Notebook::from_value(nb).unwrap().cells.remove(0)
        };
        let md = |s: &str| {
            cell(serde_json::json!({"cell_type": "markdown", "id": "x",
                "metadata": {}, "source": s}))
        };
        assert!(is_request(&md("@agent check the 2019 rows"), "claude-code"));
        assert!(is_request(&md("  @Claude, why?"), "claude-code"));
        assert!(is_request(&md("@claude-code why"), "claude-code"));
        assert!(is_request(&md("@agent"), "x"));
        assert!(!is_request(&md("@agents are"), "x"), "another word");
        assert!(!is_request(&md("see @agent"), "x"), "not first");
        assert!(!is_request(&md("@someone"), "claude-code"));
        let code = cell(serde_json::json!({"cell_type": "code", "id": "y",
            "metadata": {}, "source": "@agent", "outputs": [], "execution_count": null}));
        assert!(!is_request(&code, "x"), "only Markdown");
        let answered = |id: &str| {
            cell(serde_json::json!({"cell_type": "markdown", "id": id,
                "metadata": {"ganymede": {"request": {"done": "t", "answer": "a", "id": "x"}}},
                "source": "@agent why?"}))
        };
        assert!(!is_request(&answered("x"), "x"), "answered");
        assert!(
            is_request(&answered("copy"), "x"),
            "a copy of an answered request"
        );
    }

    #[test]
    fn marks_forget_the_least_recently_used() {
        let mut m = Marks::default();
        for n in 0..LIMIT {
            m.set(&n.to_string(), "p", Vec::new(), HashMap::new());
        }
        assert!(m.get("0").is_some());
        m.set("new", "p", Vec::new(), HashMap::new());
        assert!(m.get("0").is_some(), "used last, kept");
        assert!(m.get("1").is_none(), "least recently used, dropped");
        assert_eq!(m.marks.len(), LIMIT);
    }
}
