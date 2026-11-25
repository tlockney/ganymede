//! Recovery copies (#945): a room's notebook kept in a directory of its
//! own when the room is about to lose edits the file doesn't have, because
//! its saves failed, its file is gone, it is read-only, or the disk won a
//! same-cell merge.
//!
//! A copy is two files in the recovery dir, `<id>.ipynb` (the notebook as
//! a save would write it) and `<id>.json` (its [`Meta`]). Ids are made of
//! `[0-9a-f]` and one `-`, sort by time, and are the only names the HTTP
//! routes accept. Writes go through a temporary file and a rename, the
//! notebook first, so a `.json` always has its `.ipynb`.
//!
//! The dir is bounded: [`MAX_PER_PATH`] copies of one notebook path and
//! [`MAX_TOTAL_BYTES`] in all. A write that goes over deletes the oldest
//! copies first and logs each.

use ganymede_nbformat::Notebook;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// Copies kept for one notebook path.
pub(crate) const MAX_PER_PATH: usize = 20;
/// Bytes kept in the whole directory.
pub(crate) const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024;

/// Why a copy was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The room's saves kept failing.
    SaveFailed,
    /// The file was deleted under the room.
    FileGone,
    /// Another server owns the file, so the room can't save.
    ReadOnly,
    /// The disk won a cell the room had edited.
    MergeConflict,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::SaveFailed => "save_failed",
            Reason::FileGone => "file_gone",
            Reason::ReadOnly => "read_only",
            Reason::MergeConflict => "merge_conflict",
        }
    }
}

/// What `<id>.json` holds. `cells` are the ids of the cells a merge
/// conflict was about; empty for the other reasons.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct Meta {
    #[serde(default)]
    pub id: String,
    pub path: String,
    pub at: String,
    pub reason: String,
    pub detail: String,
    #[serde(default)]
    pub cells: Vec<String>,
    pub notebook: String,
}

impl Meta {
    /// What a room puts in `state.recovery`.
    pub(crate) fn summary(&self) -> Value {
        json!({"id": self.id, "at": self.at, "reason": self.reason, "cells": self.cells})
    }
}

/// Whether `id` is one [`write`] makes: 16 hex digits of microseconds,
/// `-`, 8 hex digits. Anything else is never turned into a file name.
pub(crate) fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 25
        && b[16] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 16 || matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn new_id() -> String {
    let micros = jiff::Timestamp::now().as_microsecond().max(0);
    let random = uuid::Uuid::new_v4().as_u128() as u32;
    format!("{micros:016x}-{random:08x}")
}

/// Write `data` to `dir/name` through a temporary file in `dir`.
fn put(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    ganymede_nbformat::atomic_write(dir.join(name), data)
}

/// Keep `notebook` (the room's, as of now) as a recovery copy of `path`.
/// `cells` are the ids a merge conflict was about, `detail` the last save
/// error or the conflicting parts. Returns the copy's meta.
pub(crate) fn write(
    dir: &Path,
    path: &str,
    notebook: &Notebook,
    reason: Reason,
    detail: &str,
    cells: &[String],
) -> io::Result<Meta> {
    std::fs::create_dir_all(dir)?;
    let id = new_id();
    let meta = Meta {
        id: id.clone(),
        path: path.to_string(),
        at: jiff::Timestamp::now().to_string(),
        reason: reason.as_str().to_string(),
        detail: detail.to_string(),
        cells: cells.to_vec(),
        notebook: format!("{id}.ipynb"),
    };
    put(dir, &meta.notebook, notebook.to_file_string().as_bytes())?;
    let json = serde_json::to_vec_pretty(&meta).map_err(io::Error::other)?;
    if let Err(e) = put(dir, &format!("{id}.json"), &json) {
        // No meta, no copy: don't leave the notebook behind.
        if let Err(rm) = std::fs::remove_file(dir.join(&meta.notebook)) {
            tracing::warn!(
                id,
                "cannot remove a recovery notebook left without its meta: {rm}"
            );
        }
        return Err(e);
    }
    prune(dir, MAX_TOTAL_BYTES);
    Ok(meta)
}

/// Every copy in `dir`, newest first. A directory that doesn't exist has
/// none; a meta that can't be read is logged and left out.
fn all(dir: &Path) -> io::Result<Vec<Meta>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut metas = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if !valid_id(id) {
            continue;
        }
        match read_meta(dir, id) {
            Ok(Some(meta)) => metas.push(meta),
            Ok(None) => {}
            Err(e) => tracing::warn!(id, "unreadable recovery copy: {e}"),
        }
    }
    metas.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(metas)
}

fn read_meta(dir: &Path, id: &str) -> io::Result<Option<Meta>> {
    if !valid_id(id) {
        return Ok(None);
    }
    let bytes = match std::fs::read(dir.join(format!("{id}.json"))) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut meta: Meta = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    meta.id = id.to_string();
    Ok(Some(meta))
}

/// The copies of notebook `path`, newest first.
pub(crate) fn list(dir: &Path, path: &str) -> io::Result<Vec<Meta>> {
    let mut metas = all(dir)?;
    metas.retain(|m| m.path == path);
    Ok(metas)
}

/// The copy `id` with its notebook, or `None` when there is none (or `id`
/// is not one of [`write`]'s).
pub(crate) fn read(dir: &Path, id: &str) -> io::Result<Option<(Meta, Value)>> {
    let Some(meta) = read_meta(dir, id)? else {
        return Ok(None);
    };
    let bytes = match std::fs::read(dir.join(format!("{id}.ipynb"))) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let content = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    Ok(Some((meta, content)))
}

/// Delete the copy `id`. Whether there was one.
pub(crate) fn discard(dir: &Path, id: &str) -> io::Result<bool> {
    if !valid_id(id) {
        return Ok(false);
    }
    // The meta first: a copy without one is not listed.
    match std::fs::remove_file(dir.join(format!("{id}.json"))) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    match std::fs::remove_file(dir.join(format!("{id}.ipynb"))) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

fn size_of(dir: &Path, meta: &Meta) -> u64 {
    [format!("{}.json", meta.id), meta.notebook.clone()]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(n)).ok())
        .map(|m| m.len())
        .sum()
}

/// Delete the oldest copies of a path over [`MAX_PER_PATH`], then the
/// oldest of all while the directory is over `max_bytes`, always
/// keeping the newest copy. Failures are logged: the copy just written is
/// safe either way.
fn prune(dir: &Path, max_bytes: u64) {
    let metas = match all(dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("cannot list recovery copies to prune them: {e}");
            return;
        }
    };
    let mut doomed: Vec<&Meta> = Vec::new();
    let mut per_path: HashMap<&str, usize> = HashMap::new();
    let mut kept: Vec<&Meta> = Vec::new();
    // Newest first: a path's copies past the limit are its oldest.
    for m in &metas {
        let n = per_path.entry(m.path.as_str()).or_default();
        *n += 1;
        if *n > MAX_PER_PATH {
            doomed.push(m);
        } else {
            kept.push(m);
        }
    }
    let mut total: u64 = kept.iter().map(|m| size_of(dir, m)).sum();
    while total > max_bytes && kept.len() > 1 {
        let oldest = kept.pop().expect("more than one");
        total -= size_of(dir, oldest);
        doomed.push(oldest);
    }
    for m in doomed {
        tracing::warn!(id = %m.id, path = %m.path, "recovery copy over the limit; deleting it");
        if let Err(e) = discard(dir, &m.id) {
            tracing::warn!(id = %m.id, "cannot delete a recovery copy: {e}");
        }
    }
}

/// `dir` under the data dir that holds a server's recovery copies.
pub(crate) fn dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("recovery")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(text: &str) -> Notebook {
        Notebook::from_value(json!({"cells": [{"cell_type": "markdown", "id": "a",
            "metadata": {}, "source": text}], "metadata": {}, "nbformat": 4,
            "nbformat_minor": 5}))
        .unwrap()
    }

    #[test]
    fn ids_are_the_ones_the_generator_makes() {
        assert!(valid_id(&new_id()));
        for bad in [
            "",
            "../0000000000000000-00000000",
            "0000000000000000-0000000/",
            "0000000000000000-0000000g",
            "0000000000000000_00000000",
            "0000000000000000-00000000.json",
            "0000000000000000-0000000A",
            "..%2f..%2f..%2f..%2f..%2f..%2f",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_copy_has_its_notebook_and_meta() {
        let dir = tempfile::tempdir().unwrap();
        let meta = write(
            dir.path(),
            "d/a.ipynb",
            &nb("mine"),
            Reason::SaveFailed,
            "disk full",
            &[],
        )
        .unwrap();
        assert!(valid_id(&meta.id));
        let (read_meta, content) = read(dir.path(), &meta.id).unwrap().unwrap();
        assert_eq!(read_meta, meta);
        assert_eq!(read_meta.reason, "save_failed");
        assert_eq!(content["cells"][0]["source"], json!(["mine"]));
        assert_eq!(
            list(dir.path(), "d/a.ipynb").unwrap(),
            std::slice::from_ref(&meta)
        );
        assert!(list(dir.path(), "other.ipynb").unwrap().is_empty());
        assert!(discard(dir.path(), &meta.id).unwrap());
        assert!(!discard(dir.path(), &meta.id).unwrap());
        assert!(read(dir.path(), &meta.id).unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// The size bound deletes the oldest copies first and keeps the newest
    /// even when it alone is over.
    #[test]
    fn the_size_bound_deletes_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<String> = (0..3)
            .map(|i| {
                write(
                    dir.path(),
                    &format!("{i}.ipynb"),
                    &nb("x"),
                    Reason::SaveFailed,
                    "",
                    &[],
                )
                .unwrap()
                .id
            })
            .collect();
        let one = size_of(dir.path(), &all(dir.path()).unwrap()[0]);
        prune(dir.path(), one * 2);
        let left: Vec<String> = all(dir.path()).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(left, [ids[2].clone(), ids[1].clone()]);
        prune(dir.path(), 1);
        let left: Vec<String> = all(dir.path()).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(left, [ids[2].clone()]);
    }

    /// The bound: the 21st copy of a path deletes the oldest.
    #[test]
    fn twenty_one_copies_of_one_path_leave_twenty() {
        let dir = tempfile::tempdir().unwrap();
        let mut ids = Vec::new();
        for i in 0..MAX_PER_PATH + 1 {
            let m = write(
                dir.path(),
                "a.ipynb",
                &nb(&format!("v{i}")),
                Reason::SaveFailed,
                "",
                &[],
            )
            .unwrap();
            ids.push(m.id);
        }
        // Another path's copy is not counted against it.
        write(dir.path(), "b.ipynb", &nb("b"), Reason::FileGone, "", &[]).unwrap();
        let left: Vec<String> = list(dir.path(), "a.ipynb")
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left.len(), MAX_PER_PATH);
        assert!(!left.contains(&ids[0]), "the oldest is gone");
        assert!(left.contains(&ids[MAX_PER_PATH]), "the newest stays");
        assert!(!dir.path().join(format!("{}.ipynb", ids[0])).exists());
        assert_eq!(list(dir.path(), "b.ipynb").unwrap().len(), 1);
    }
}
