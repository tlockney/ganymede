//! Three-way merge of notebooks, for a file that changed on disk while a
//! room held unsaved edits.
//!
//! `base` is the file as the room last read or wrote it, `ours` the room's
//! content, `theirs` the file now. Cells are matched by id. A cell added
//! on either side is kept, after the cell it followed there; a cell deleted
//! on one side and untouched on the other is dropped. A cell on all sides
//! merges part by part: its source, its outputs with its execution count,
//! each metadata key, and each other key. So a source edit in the room
//! survives an agent's run that changed only that cell's outputs on disk.
//! Notebook-level fields (`nbformat_minor`, each `metadata` key) merge the
//! same way, key by key.
//!
//! When both sides changed the same part differently, or deleted a cell
//! the other changed, or both reordered cells, the disk wins for that
//! part, as `jupyter-collaboration` lets the disk win for the whole
//! document. Each cell or top-level key with such a part is listed in
//! [`Merged::conflicts`].
//!
//! Cells without ids (nbformat 4.4 and older) can't be matched; the disk
//! wins for the whole document.

use serde_json::{Map as JsonMap, Value};
use std::collections::{HashMap, HashSet};

/// The result of [`merge`].
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub notebook: Value,
    /// What the room's edits lost to the disk: `cell <id>`, a top-level
    /// key such as `metadata`, `order`, or `all` (cells without ids).
    pub conflicts: Vec<String>,
}

/// Merge `ours` and `theirs`, both descended from `base`. All three are
/// notebooks as JSON (see [`crate::schema::notebook_value`]).
pub fn merge(base: &Value, ours: &Value, theirs: &Value) -> Merged {
    let mut conflicts = Vec::new();
    let (Some(b), Some(o), Some(t)) = (base.as_object(), ours.as_object(), theirs.as_object())
    else {
        return disk_wins(theirs);
    };
    let (Some(bc), Some(oc), Some(tc)) = (cells(b), cells(o), cells(t)) else {
        return disk_wins(theirs);
    };

    let mut top = JsonMap::new();
    let keys: Vec<&String> = {
        let mut seen = HashSet::new();
        t.keys()
            .chain(o.keys())
            .chain(b.keys())
            .filter(|k| k.as_str() != "cells" && seen.insert(k.as_str()))
            .collect()
    };
    for key in keys {
        let (value, conflict) = if key == "metadata" {
            merge_by_key(b.get(key), o.get(key), t.get(key))
        } else {
            let pick = three_way(b.get(key), o.get(key), t.get(key));
            (pick.value.cloned(), pick.conflict)
        };
        if conflict {
            conflicts.push(key.clone());
        }
        if let Some(v) = value {
            top.insert(key.clone(), v);
        }
    }

    let base_by: HashMap<&str, &Value> = bc.iter().map(|(id, c)| (*id, *c)).collect();
    let ours_by: HashMap<&str, &Value> = oc.iter().map(|(id, c)| (*id, *c)).collect();
    let theirs_by: HashMap<&str, &Value> = tc.iter().map(|(id, c)| (*id, *c)).collect();

    // Which cells survive, and with what content.
    let mut kept: HashMap<&str, Value> = HashMap::new();
    let all_ids: Vec<&str> = {
        let mut seen = HashSet::new();
        tc.iter()
            .chain(oc.iter())
            .chain(bc.iter())
            .map(|(id, _)| *id)
            .filter(|id| seen.insert(*id))
            .collect()
    };
    for id in all_ids {
        let (value, conflict) = merge_cell(
            base_by.get(id).copied(),
            ours_by.get(id).copied(),
            theirs_by.get(id).copied(),
        );
        if conflict {
            conflicts.push(format!("cell {id}"));
        }
        if let Some(v) = value {
            kept.insert(id, v);
        }
    }

    // Order: whichever side kept the base order of the cells all three
    // share gives way to the other; the other side's additions go after
    // the cell they followed there.
    let shared = |list: &[(&str, &Value)]| -> Vec<String> {
        list.iter()
            .map(|(id, _)| *id)
            .filter(|id| {
                base_by.contains_key(id) && ours_by.contains_key(id) && theirs_by.contains_key(id)
            })
            .map(str::to_string)
            .collect()
    };
    let (sb, so, st) = (shared(&bc), shared(&oc), shared(&tc));
    let (skeleton, other) = if so == sb {
        (&tc, &oc)
    } else if st == sb || st == so {
        (&oc, &tc)
    } else {
        conflicts.push("order".into());
        (&tc, &oc)
    };
    let mut order: Vec<&str> = skeleton
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| kept.contains_key(id))
        .collect();
    let mut placed: HashSet<&str> = order.iter().copied().collect();
    let mut after: Option<&str> = None;
    for (id, _) in other.iter() {
        if kept.contains_key(id) && !placed.contains(id) {
            let at = match after {
                Some(prev) => order.iter().position(|x| *x == prev).map_or(0, |i| i + 1),
                None => 0,
            };
            order.insert(at, id);
            placed.insert(id);
        }
        if placed.contains(id) {
            after = Some(id);
        }
    }
    let cells: Vec<Value> = order
        .iter()
        .map(|id| kept.remove(id).expect("kept"))
        .collect();
    top.insert("cells".into(), Value::Array(cells));
    reorder_like(&mut top, t);
    Merged {
        notebook: Value::Object(top),
        conflicts,
    }
}

/// One cell's merge. A cell on all three sides merges part by part, so an
/// edit to its source survives a disk change to only its outputs: the
/// source; the outputs with the execution count (they change together
/// when it runs); the metadata, key by key; and any other key on its own.
/// A change of cell type, or a cell added or deleted, merges whole.
fn merge_cell(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
) -> (Option<Value>, bool) {
    let whole = || {
        let pick = three_way(base, ours, theirs);
        (pick.value.cloned(), pick.conflict)
    };
    let (Some(b), Some(o), Some(t)) = (
        base.and_then(Value::as_object),
        ours.and_then(Value::as_object),
        theirs.and_then(Value::as_object),
    ) else {
        return whole();
    };
    if b.get("cell_type") != o.get("cell_type") || b.get("cell_type") != t.get("cell_type") {
        return whole();
    }
    let mut cell = JsonMap::new();
    let mut conflict = false;
    // Outputs and execution count, as one part: the keys present of the two.
    let run = |c: &JsonMap<String, Value>| -> Value {
        let mut part = JsonMap::new();
        for key in RUN {
            if let Some(v) = c.get(key) {
                part.insert(key.to_string(), v.clone());
            }
        }
        Value::Object(part)
    };
    let (rb, ro, rt) = (run(b), run(o), run(t));
    let pick = three_way(Some(&rb), Some(&ro), Some(&rt));
    conflict |= pick.conflict;
    if let Some(Value::Object(part)) = pick.value {
        cell.extend(part.clone());
    }
    let mut seen = HashSet::new();
    for key in t.keys().chain(o.keys()).chain(b.keys()) {
        if RUN.contains(&key.as_str()) || !seen.insert(key.as_str()) {
            continue;
        }
        let (value, c) = if key == "metadata" {
            merge_by_key(b.get(key), o.get(key), t.get(key))
        } else {
            let pick = three_way(b.get(key), o.get(key), t.get(key));
            (pick.value.cloned(), pick.conflict)
        };
        conflict |= c;
        if let Some(v) = value {
            cell.insert(key.clone(), v);
        }
    }
    reorder_like(&mut cell, t);
    (Some(Value::Object(cell)), conflict)
}

/// A cell's keys that change together when it runs.
const RUN: [&str; 2] = ["outputs", "execution_count"];

/// An object's merge, key by key (notebook and cell metadata). Anything
/// but three objects merges whole.
fn merge_by_key(
    base: Option<&Value>,
    ours: Option<&Value>,
    theirs: Option<&Value>,
) -> (Option<Value>, bool) {
    let empty = JsonMap::new();
    fn as_map<'a>(
        v: Option<&'a Value>,
        empty: &'a JsonMap<String, Value>,
    ) -> Option<&'a JsonMap<String, Value>> {
        match v {
            None => Some(empty),
            Some(Value::Object(m)) => Some(m),
            Some(_) => None,
        }
    }
    let (Some(b), Some(o), Some(t)) = (
        as_map(base, &empty),
        as_map(ours, &empty),
        as_map(theirs, &empty),
    ) else {
        let pick = three_way(base, ours, theirs);
        return (pick.value.cloned(), pick.conflict);
    };
    // Deleted on one side and unchanged on the other: gone. Compared as
    // given, so an empty map added where there was none is a change.
    if ours.is_none() && theirs == base || theirs.is_none() && ours == base {
        return (None, false);
    }
    let mut out = JsonMap::new();
    let mut conflict = false;
    let mut seen = HashSet::new();
    for key in t.keys().chain(o.keys()).chain(b.keys()) {
        if !seen.insert(key.as_str()) {
            continue;
        }
        let pick = three_way(b.get(key), o.get(key), t.get(key));
        conflict |= pick.conflict;
        if let Some(v) = pick.value {
            out.insert(key.clone(), v.clone());
        }
    }
    (Some(Value::Object(out)), conflict)
}

fn disk_wins(theirs: &Value) -> Merged {
    Merged {
        notebook: theirs.clone(),
        conflicts: vec!["all".into()],
    }
}

/// Cells by id, in order. `None` when a cell has no id or ids repeat.
fn cells(nb: &JsonMap<String, Value>) -> Option<Vec<(&str, &Value)>> {
    let list = match nb.get("cells") {
        Some(Value::Array(a)) => a,
        None => return Some(Vec::new()),
        Some(_) => return None,
    };
    let mut seen = HashSet::new();
    list.iter()
        .map(|c| {
            let id = c.get("id")?.as_str()?;
            seen.insert(id).then_some((id, c))
        })
        .collect()
}

struct Pick<'a> {
    value: Option<&'a Value>,
    conflict: bool,
}

/// One value's three-way merge; `None` is absent.
fn three_way<'a>(
    base: Option<&'a Value>,
    ours: Option<&'a Value>,
    theirs: Option<&'a Value>,
) -> Pick<'a> {
    if ours == base {
        Pick {
            value: theirs,
            conflict: false,
        }
    } else if theirs == base || theirs == ours {
        Pick {
            value: ours,
            conflict: false,
        }
    } else {
        Pick {
            value: theirs,
            conflict: true,
        }
    }
}

/// Put `top`'s keys in the order the file has them, so a merge doesn't
/// reorder what neither side moved.
fn reorder_like(top: &mut JsonMap<String, Value>, like: &JsonMap<String, Value>) {
    let mut out = JsonMap::new();
    for key in like.keys() {
        if let Some(v) = top.remove(key) {
            out.insert(key.clone(), v);
        }
    }
    out.append(top);
    *top = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cell(id: &str, source: &str) -> Value {
        json!({"cell_type": "markdown", "id": id, "metadata": {}, "source": source})
    }

    fn nb(cells: Vec<Value>) -> Value {
        json!({"cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5})
    }

    fn sources(v: &Value) -> Vec<String> {
        v["cells"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                format!(
                    "{}={}",
                    c["id"].as_str().unwrap(),
                    c["source"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn edits_to_different_cells_both_survive() {
        let base = nb(vec![cell("a", "1"), cell("b", "2"), cell("c", "3")]);
        let ours = nb(vec![cell("a", "1 ours"), cell("b", "2"), cell("c", "3")]);
        let theirs = nb(vec![cell("a", "1"), cell("b", "2"), cell("c", "3 disk")]);
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(sources(&m.notebook), ["a=1 ours", "b=2", "c=3 disk"]);
    }

    #[test]
    fn same_cell_changed_on_both_sides_takes_the_disk() {
        let base = nb(vec![cell("a", "1"), cell("b", "2")]);
        let ours = nb(vec![cell("a", "ours"), cell("b", "2 ours")]);
        let theirs = nb(vec![cell("a", "disk"), cell("b", "2")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["cell a"]);
        assert_eq!(sources(&m.notebook), ["a=disk", "b=2 ours"]);
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let base = nb(vec![cell("a", "1")]);
        let both = nb(vec![cell("a", "2")]);
        let m = merge(&base, &both, &both);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.notebook, both);
    }

    #[test]
    fn additions_keep_their_place_and_deletions_apply() {
        let base = nb(vec![cell("a", "1"), cell("b", "2"), cell("c", "3")]);
        // Room adds x after a and deletes c; the disk adds y after b.
        let ours = nb(vec![cell("a", "1"), cell("x", "new"), cell("b", "2")]);
        let theirs = nb(vec![
            cell("a", "1"),
            cell("b", "2"),
            cell("y", "disk"),
            cell("c", "3"),
        ]);
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(sources(&m.notebook), ["a=1", "x=new", "b=2", "y=disk"]);
    }

    #[test]
    fn a_cell_added_first_goes_first() {
        let base = nb(vec![cell("a", "1")]);
        let ours = nb(vec![cell("x", "top"), cell("a", "1")]);
        let theirs = nb(vec![cell("a", "1 disk")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(sources(&m.notebook), ["x=top", "a=1 disk"]);
    }

    #[test]
    fn deleting_a_cell_the_other_side_changed_takes_the_disk() {
        let base = nb(vec![cell("a", "1"), cell("b", "2")]);
        let ours = nb(vec![cell("a", "1")]);
        let theirs = nb(vec![cell("a", "1"), cell("b", "2 disk")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["cell b"]);
        assert_eq!(sources(&m.notebook), ["a=1", "b=2 disk"]);

        // And the other way round: deleted on disk, edited in the room.
        let ours = nb(vec![cell("a", "1"), cell("b", "2 ours")]);
        let theirs = nb(vec![cell("a", "1")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["cell b"]);
        assert_eq!(sources(&m.notebook), ["a=1"]);
    }

    #[test]
    fn a_move_on_one_side_merges_with_an_edit_on_the_other() {
        let base = nb(vec![cell("a", "1"), cell("b", "2"), cell("c", "3")]);
        let ours = nb(vec![cell("c", "3"), cell("a", "1"), cell("b", "2")]);
        let theirs = nb(vec![cell("a", "1 disk"), cell("b", "2"), cell("c", "3")]);
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(sources(&m.notebook), ["c=3", "a=1 disk", "b=2"]);
    }

    #[test]
    fn two_different_reorders_take_the_disk_order() {
        let base = nb(vec![cell("a", "1"), cell("b", "2"), cell("c", "3")]);
        let ours = nb(vec![cell("c", "3"), cell("a", "1"), cell("b", "2")]);
        let theirs = nb(vec![cell("b", "2"), cell("a", "1"), cell("c", "3")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["order"]);
        assert_eq!(sources(&m.notebook), ["b=2", "a=1", "c=3"]);
    }

    #[test]
    fn metadata_merges_by_key() {
        let base = nb(vec![]);
        let mut ours = base.clone();
        ours["metadata"] = json!({"title": "ours"});
        let mut theirs = base.clone();
        theirs["nbformat_minor"] = json!(5);
        theirs["metadata"] = json!({"title": "disk"});
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["metadata"]);
        assert_eq!(m.notebook["metadata"], json!({"title": "disk"}));

        let mut theirs = base.clone();
        theirs["extra"] = json!(true);
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.notebook["metadata"], json!({"title": "ours"}));
        assert_eq!(m.notebook["extra"], json!(true));
    }

    #[test]
    fn cells_without_ids_take_the_disk() {
        let base = json!({"cells": [{"cell_type": "markdown", "metadata": {}, "source": "1"}],
                          "metadata": {}, "nbformat": 4, "nbformat_minor": 4});
        let mut ours = base.clone();
        ours["cells"][0]["source"] = json!("ours");
        let mut theirs = base.clone();
        theirs["metadata"] = json!({"x": 1});
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["all"]);
        assert_eq!(m.notebook, theirs);
    }

    fn code(id: &str, source: &str, out: &str, count: Value) -> Value {
        json!({"cell_type": "code", "id": id, "metadata": {}, "source": source, "execution_count": count,
               "outputs": [{"output_type": "stream", "name": "stdout", "text": out}]})
    }

    #[test]
    fn a_source_edit_survives_a_run_on_disk() {
        // The person types in the room; an agent runs the cell on disk.
        let base = nb(vec![code("a", "print(1)", "", Value::Null)]);
        let ours = nb(vec![code("a", "print(2)", "", Value::Null)]);
        let theirs = nb(vec![code("a", "print(1)", "1\n", json!(3))]);
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(
            m.notebook["cells"][0],
            code("a", "print(2)", "1\n", json!(3))
        );

        // And the other way round: the room ran it, the disk edited it.
        let m = merge(&base, &theirs, &ours);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(
            m.notebook["cells"][0],
            code("a", "print(2)", "1\n", json!(3))
        );
    }

    #[test]
    fn outputs_and_count_merge_as_one_part() {
        // Both sides ran the cell: the disk's run wins whole, not a mix
        // of one side's outputs and the other's count.
        let base = nb(vec![code("a", "x", "", Value::Null)]);
        let ours = nb(vec![code("a", "x", "ours", json!(1))]);
        let theirs = nb(vec![code("a", "x", "disk", json!(7))]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["cell a"]);
        assert_eq!(m.notebook["cells"][0], code("a", "x", "disk", json!(7)));
    }

    #[test]
    fn cell_metadata_merges_by_key() {
        let mut base = cell("a", "1");
        base["metadata"] = json!({"tags": ["t"]});
        let mut ours = base.clone();
        ours["metadata"]["collapsed"] = json!(true);
        let mut theirs = base.clone();
        theirs["metadata"]["tags"] = json!(["t", "u"]);
        theirs["source"] = json!("1 disk");
        let m = merge(&nb(vec![base]), &nb(vec![ours]), &nb(vec![theirs]));
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        let merged = &m.notebook["cells"][0];
        assert_eq!(
            merged["metadata"],
            json!({"tags": ["t", "u"], "collapsed": true})
        );
        assert_eq!(merged["source"], "1 disk");
    }

    #[test]
    fn a_changed_cell_type_merges_whole() {
        let base = nb(vec![cell("a", "1")]);
        let mut ours = base.clone();
        ours["cells"][0]["cell_type"] = json!("raw");
        let theirs = nb(vec![cell("a", "1 disk")]);
        let m = merge(&base, &ours, &theirs);
        assert_eq!(m.conflicts, ["cell a"]);
        assert_eq!(m.notebook, theirs);
    }

    #[test]
    fn an_added_empty_map_is_kept() {
        let empty = json!({});
        assert_eq!(
            merge_by_key(None, None, Some(&empty)),
            (Some(json!({})), false)
        );
        assert_eq!(
            merge_by_key(None, Some(&empty), None),
            (Some(json!({})), false)
        );
        assert_eq!(
            merge_by_key(Some(&empty), None, Some(&empty)),
            (None, false)
        );
    }

    #[test]
    fn notebook_metadata_merges_by_key() {
        let mut base = nb(vec![]);
        base["metadata"] = json!({"kernelspec": {"name": "a"}});
        let mut ours = base.clone();
        ours["metadata"]["title"] = json!("ours");
        let mut theirs = base.clone();
        theirs["metadata"]["kernelspec"] = json!({"name": "b"});
        let m = merge(&base, &ours, &theirs);
        assert!(m.conflicts.is_empty(), "{:?}", m.conflicts);
        assert_eq!(
            m.notebook["metadata"],
            json!({"kernelspec": {"name": "b"}, "title": "ours"})
        );
    }

    #[test]
    fn unchanged_cells_keep_their_key_order() {
        let base = nb(vec![code("a", "x", "", Value::Null)]);
        let mut ours = base.clone();
        ours["cells"][0]["source"] = json!("y");
        let m = merge(&base, &ours, &base);
        let keys: Vec<&String> = m.notebook["cells"][0].as_object().unwrap().keys().collect();
        let want: Vec<&String> = base["cells"][0].as_object().unwrap().keys().collect();
        assert_eq!(keys, want);
    }
}
