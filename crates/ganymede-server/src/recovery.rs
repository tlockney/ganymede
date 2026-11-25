//! Recovery copies (#945): a room's notebook kept in a directory of its
//! own when the room is about to lose edits the file doesn't have, because
//! its saves failed, its file is gone, it is read-only, or the disk won a
//! same-cell merge.
//!
//! All servers of a person share one recovery dir. Each root has its own
//! dir in it ([`Store`]), so a server sees, offers and prunes only the
//! copies of the notebooks it serves.
//!
//! A copy is two files in its root's dir, `<id>.ipynb` (the notebook as
//! a save would write it) and `<id>.json` (its [`Meta`]). Ids are made of
//! `[0-9a-f]` and one `-`, sort by time, and are the only names the HTTP
//! routes accept. Writes go through a temporary file and a rename, the
//! notebook first, so a `.json` always has its `.ipynb`.
//!
//! A root's dir is bounded: [`MAX_PER_PATH`] copies of one notebook path and
//! [`MAX_TOTAL_BYTES`] in all. A write that goes over deletes the oldest
//! copies first and logs each.

use ganymede_nbformat::Notebook;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Copies kept for one notebook path.
pub(crate) const MAX_PER_PATH: usize = 20;
/// Bytes kept in one root's directory.
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
    /// The notebook's API path, relative to its server's root.
    pub path: String,
    /// The notebook file's absolute path, where the server has a root. Not
    /// sent to clients, which know notebooks by `path`.
    #[serde(default)]
    pub file: Option<PathBuf>,
    pub at: String,
    pub reason: String,
    pub detail: String,
    #[serde(default)]
    pub cells: Vec<String>,
    pub notebook: String,
}

/// One root's copies.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    /// The root's dir in the recovery dir. Made by the first copy.
    pub dir: PathBuf,
    root: Option<PathBuf>,
}

impl Store {
    /// The copies of the notebooks under `root`, in `recovery`.
    pub(crate) fn new(recovery: &Path, root: Option<&Path>) -> Store {
        let root = root.map(|r| r.canonicalize().unwrap_or_else(|_| r.to_path_buf()));
        let bytes = root.as_ref().map(|r| r.as_os_str().as_encoded_bytes());
        let key = hex::encode(Sha256::digest(bytes.unwrap_or_default()));
        Store {
            dir: recovery.join(key),
            root,
        }
    }
}

impl Meta {
    /// What the HTTP routes answer: everything but `file`.
    pub(crate) fn public(&self) -> Value {
        let mut meta = serde_json::to_value(self).expect("a meta serializes");
        if let Value::Object(m) = &mut meta {
            m.remove("file");
        }
        meta
    }

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

/// An id later than every id made before it in this process, whatever the
/// clock does.
fn new_id() -> String {
    static LAST: Mutex<i64> = Mutex::new(-1);
    let micros = {
        let mut last = LAST.lock().expect("the last id's time");
        *last = jiff::Timestamp::now().as_microsecond().max(*last + 1);
        *last
    };
    let random = uuid::Uuid::new_v4().as_u128() as u32;
    format!("{micros:016x}-{random:08x}")
}

/// Write `data` to `dir/name` through a temporary file in `dir`.
fn put(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    ganymede_nbformat::atomic_write(dir.join(name), data)
}

/// Keep `notebook` (the room's, as of now) as a recovery copy of `path`.
/// `cells` are the ids a merge conflict was about, `detail` the last save
/// error or the conflicting parts. Returns the copy's meta, and those of
/// the copies deleted to stay in bounds.
pub(crate) fn write(
    store: &Store,
    path: &str,
    notebook: &Notebook,
    reason: Reason,
    detail: &str,
    cells: &[String],
) -> io::Result<(Meta, Vec<Meta>)> {
    let dir = store.dir.as_path();
    std::fs::create_dir_all(dir)?;
    let id = new_id();
    let meta = Meta {
        id: id.clone(),
        path: path.to_string(),
        file: store.root.as_ref().map(|r| r.join(path)),
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
    let pruned = prune(dir, MAX_TOTAL_BYTES);
    Ok((meta, pruned))
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

/// The meta of copy `id`, or `None` when there is none (or `id` is not one
/// of [`write`]'s).
pub(crate) fn read_meta(dir: &Path, id: &str) -> io::Result<Option<Meta>> {
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
/// safe either way. Returns the copies deleted.
fn prune(dir: &Path, max_bytes: u64) -> Vec<Meta> {
    let metas = match all(dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("cannot list recovery copies to prune them: {e}");
            return Vec::new();
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
    let mut deleted = Vec::new();
    for m in doomed {
        tracing::warn!(id = %m.id, path = %m.path, "recovery copy over the limit; deleting it");
        match discard(dir, &m.id) {
            Ok(_) => deleted.push(m.clone()),
            Err(e) => tracing::warn!(id = %m.id, "cannot delete a recovery copy: {e}"),
        }
    }
    deleted
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

    /// A store in `dir` for a server with no root.
    fn store(dir: &Path) -> Store {
        Store::new(dir, None)
    }

    /// Ids sort in the order they were made, in one thread and across
    /// threads, however many are made in a microsecond.
    #[test]
    fn ids_strictly_increase() {
        let made = || (0..2000).map(|_| new_id()).collect::<Vec<_>>();
        let threads: Vec<_> = (0..4).map(|_| std::thread::spawn(made)).collect();
        let mut times = std::collections::HashSet::new();
        for t in threads {
            let ids = t.join().unwrap();
            assert!(ids.windows(2).all(|w| w[0] < w[1]));
            for id in ids {
                assert!(times.insert(id[..16].to_string()), "{id}'s time twice");
            }
        }
    }

    /// Two roots in one recovery dir, with the same notebook path in each.
    #[test]
    fn roots_do_not_see_each_others_copies() {
        let rec = tempfile::tempdir().unwrap();
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let [one, two] = [0, 1].map(|i| Store::new(rec.path(), Some(roots[i].path())));
        let put = |s: &Store, text: &str| {
            write(s, "a.ipynb", &nb(text), Reason::SaveFailed, "", &[]).unwrap()
        };
        let mine = put(&one, "one").0;
        let file = roots[0].path().canonicalize().unwrap().join("a.ipynb");
        assert_eq!(mine.file.as_deref(), Some(file.as_path()));
        assert!(mine.public().get("file").is_none());
        // Enough copies of the same path in the other root to prune.
        let pruned: Vec<Meta> = (0..MAX_PER_PATH + 1)
            .flat_map(|_| put(&two, "two").1)
            .collect();
        assert_eq!(pruned.len(), 1);
        assert_eq!(list(&one.dir, "a.ipynb").unwrap(), [mine.clone()]);
        assert_eq!(list(&two.dir, "a.ipynb").unwrap().len(), MAX_PER_PATH);
        assert!(read(&two.dir, &mine.id).unwrap().is_none());
        assert!(!discard(&two.dir, &mine.id).unwrap());
        assert!(read(&one.dir, &mine.id).unwrap().is_some());
        // The same root by another spelling is the same root.
        let again = Store::new(rec.path(), Some(&roots[0].path().join(".")));
        assert_eq!(again.dir, one.dir);
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
        let tmp = tempfile::tempdir().unwrap();
        let copies = store(tmp.path());
        let (meta, pruned) = write(
            &copies,
            "d/a.ipynb",
            &nb("mine"),
            Reason::SaveFailed,
            "disk full",
            &[],
        )
        .unwrap();
        assert!(pruned.is_empty());
        assert!(valid_id(&meta.id));
        let (read_meta, content) = read(&copies.dir, &meta.id).unwrap().unwrap();
        assert_eq!(read_meta, meta);
        assert_eq!(read_meta.reason, "save_failed");
        assert_eq!(content["cells"][0]["source"], json!(["mine"]));
        assert_eq!(
            list(&copies.dir, "d/a.ipynb").unwrap(),
            std::slice::from_ref(&meta)
        );
        assert!(list(&copies.dir, "other.ipynb").unwrap().is_empty());
        assert!(discard(&copies.dir, &meta.id).unwrap());
        assert!(!discard(&copies.dir, &meta.id).unwrap());
        assert!(read(&copies.dir, &meta.id).unwrap().is_none());
        assert_eq!(std::fs::read_dir(&copies.dir).unwrap().count(), 0);
    }

    /// The size bound deletes the oldest copies first and keeps the newest
    /// even when it alone is over.
    #[test]
    fn the_size_bound_deletes_the_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        let copies = store(tmp.path());
        let ids: Vec<String> = (0..3)
            .map(|i| {
                write(
                    &copies,
                    &format!("{i}.ipynb"),
                    &nb("x"),
                    Reason::SaveFailed,
                    "",
                    &[],
                )
                .unwrap()
                .0
                .id
            })
            .collect();
        // Copies differ in size by the digits of their times.
        let newest = &all(&copies.dir).unwrap()[..2];
        let two = newest.iter().map(|m| size_of(&copies.dir, m)).sum();
        let pruned = prune(&copies.dir, two);
        assert_eq!(pruned.len(), 1, "it says what it deleted");
        assert_eq!(pruned[0].id, ids[0]);
        let left: Vec<String> = all(&copies.dir).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(left, [ids[2].clone(), ids[1].clone()]);
        prune(&copies.dir, 1);
        let left: Vec<String> = all(&copies.dir).unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(left, [ids[2].clone()]);
    }

    /// The bound: the 21st copy of a path deletes the oldest.
    #[test]
    fn twenty_one_copies_of_one_path_leave_twenty() {
        let tmp = tempfile::tempdir().unwrap();
        let copies = store(tmp.path());
        let mut ids = Vec::new();
        let mut pruned = Vec::new();
        for i in 0..MAX_PER_PATH + 1 {
            let (m, over) = write(
                &copies,
                "a.ipynb",
                &nb(&format!("v{i}")),
                Reason::SaveFailed,
                "",
                &[],
            )
            .unwrap();
            ids.push(m.id);
            pruned.extend(over.into_iter().map(|m| m.id));
        }
        assert_eq!(pruned, [ids[0].clone()]);
        // Another path's copy is not counted against it.
        write(&copies, "b.ipynb", &nb("b"), Reason::FileGone, "", &[]).unwrap();
        let left: Vec<String> = list(&copies.dir, "a.ipynb")
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left.len(), MAX_PER_PATH);
        assert!(!left.contains(&ids[0]), "the oldest is gone");
        assert!(left.contains(&ids[MAX_PER_PATH]), "the newest stays");
        assert!(!copies.dir.join(format!("{}.ipynb", ids[0])).exists());
        assert_eq!(list(&copies.dir, "b.ipynb").unwrap().len(), 1);
    }
}
