//! Three-way merge of notebooks, for a file that changed on disk while a
//! room held unsaved edits.
//!
//! `base` is the file as the room last read or wrote it, `ours` the room's
//! content, `theirs` the file now. Cells are matched by id. A cell changed
//! on one side only takes that side's version; a cell added on either side
//! is kept, after the cell it followed there; a cell deleted on one side
//! and untouched on the other is dropped. Notebook-level fields (metadata,
//! `nbformat_minor`) merge the same way, key by key.
//!
//! When both sides changed the same cell or key differently, or deleted a
//! cell the other changed, or both reordered cells, the disk wins for that
//! part, as `jupyter-collaboration` lets the disk win for the whole
//! document. Each such part is listed in [`Merged::conflicts`].
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
        let pick = three_way(b.get(key), o.get(key), t.get(key));
        if pick.conflict {
            conflicts.push(key.clone());
        }
        if let Some(v) = pick.value {
            top.insert(key.clone(), v.clone());
        }
    }

    let base_by: HashMap<&str, &Value> = bc.iter().map(|(id, c)| (*id, *c)).collect();
    let ours_by: HashMap<&str, &Value> = oc.iter().map(|(id, c)| (*id, *c)).collect();
    let theirs_by: HashMap<&str, &Value> = tc.iter().map(|(id, c)| (*id, *c)).collect();

    // Which cells survive, and with what content.
    let mut kept: HashMap<&str, &Value> = HashMap::new();
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
        let pick = three_way(
            base_by.get(id).copied(),
            ours_by.get(id).copied(),
            theirs_by.get(id).copied(),
        );
        if pick.conflict {
            conflicts.push(format!("cell {id}"));
        }
        if let Some(v) = pick.value {
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
    let cells: Vec<Value> = order.iter().map(|id| kept[id].clone()).collect();
    top.insert("cells".into(), Value::Array(cells));
    reorder_like(&mut top, t);
    Merged {
        notebook: Value::Object(top),
        conflicts,
    }
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
}
