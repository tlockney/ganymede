//! Recovery copies (#945): a room's notebook kept in a directory of its
//! own when the room is about to lose edits the file doesn't have, because
//! its saves failed, its file is gone, it is read-only, or the disk won a
//! same-cell merge.
//!
//! All servers of a person share one recovery dir. Each root writes its
//! copies into its own dir in it ([`Store`]). Reads match by the copy's
//! file, not its dir, so a server sees every copy of a notebook under its
//! root, whichever root's dir it is in: a copy written by a server rooted
//! at a project is also listed, read and discarded by one rooted at the
//! folder around it, at the path relative to that root. A root that holds
//! none of the copy's file doesn't see it.
//!
//! A copy is two files in a root's dir, `<id>.ipynb` (the notebook as
//! a save would write it) and `<id>.json` (its [`Meta`]). Ids are made of
//! `[0-9a-f]` and one `-`, sort by time, and are the only names the HTTP
//! routes accept. Writes go through a temporary file and a rename, the
//! notebook first, so a `.json` always has its `.ipynb`. On unix the
//! dirs and copies are private to the user: a copy holds a notebook.
//!
//! A root's dir is bounded: [`MAX_PER_PATH`] copies of one notebook path and
//! [`MAX_TOTAL_BYTES`] in all. A write that goes over deletes the oldest
//! copies first, never the one just written, and logs each.

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
    /// The notebook's API path, relative to the server that made the
    /// copy. A server with a root that holds the file sees it at the
    /// path relative to that root instead.
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
    /// The recovery dir every root's dir is in.
    recovery: PathBuf,
    /// The root's dir in the recovery dir. Made by the first copy;
    /// this server's writes go here.
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
            recovery: recovery.to_path_buf(),
            dir: recovery.join(key),
            root,
        }
    }

    /// `meta` as this server sees it: the copy's `file` is under this
    /// root, and `path` is the file's path relative to it. `None` when
    /// the root holds neither the file nor any root at all, or the copy
    /// says no file.
    fn claimed(&self, meta: Meta) -> Option<Meta> {
        let root = self.root.as_ref()?;
        let file = meta.file.as_deref()?;
        // An API path: components joined with `/`, whatever the platform's
        // separator. A hidden component (or anything but a plain name) is
        // a path the contents API refuses, so the copy isn't this
        // server's to show.
        let mut parts = Vec::new();
        for c in file.strip_prefix(root).ok()?.components() {
            let std::path::Component::Normal(name) = c else {
                return None;
            };
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                return None;
            }
            parts.push(name.into_owned());
        }
        let rel = parts.join("/");
        if rel.is_empty() {
            return None; // the root itself is not a notebook
        }
        let mut meta = meta;
        meta.path = rel;
        Some(meta)
    }

    /// The root dirs a lookup looks in: with a root, every dir under
    /// the recovery dir; with none, this store's own dir only, as the
    /// server can't tell another root's notebooks from its own.
    fn dirs(&self) -> Vec<PathBuf> {
        if self.root.is_none() {
            return vec![self.dir.clone()];
        }
        let mut dirs = Vec::new();
        match std::fs::read_dir(&self.recovery) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    // A file in the recovery dir's place, or a stray
                    // file in it, is not a root's dir.
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        dirs.push(entry.path());
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("cannot list the recovery root dirs: {e}"),
        }
        dirs
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

/// Make `dir`, and its parents, private to this user on unix (0700): it
/// holds copies of notebooks.
pub(crate) fn make_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Write `data` to `dir/name` through a temporary file in `dir`, private
/// to this user on unix (0600), as a recovery copy holds a notebook.
fn put(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let mut builder = tempfile::Builder::new();
    let prefix = format!(".~{name}-");
    builder.prefix(&prefix).suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o600));
    }
    let mut tmp = builder.tempfile_in(dir)?;
    std::io::Write::write_all(&mut tmp, data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(dir.join(name)).map_err(|e| e.error)?;
    Ok(())
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
    make_dir(dir)?;
    let id = new_id();
    let meta = Meta {
        id: id.clone(),
        path: path.to_string(),
        file: store.root.as_ref().map(|r| canonical_file(&r.join(path))),
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
    let pruned = prune(dir, MAX_TOTAL_BYTES, &id);
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
        match read_meta_in(dir, id) {
            Ok(Some(meta)) => metas.push(meta),
            Ok(None) => {}
            Err(e) => tracing::warn!(id, "unreadable recovery copy: {e}"),
        }
    }
    metas.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(metas)
}

/// Every copy this server can see, newest first: in any root's dir, the
/// ones whose file is under its root. A dir that can't be read is logged
/// and left out, not allowed to hide the rest.
fn visible(store: &Store) -> Vec<Meta> {
    let mut metas = Vec::new();
    for dir in store.dirs() {
        match all(&dir) {
            Ok(found) => metas.extend(found),
            Err(e) => tracing::warn!(dir = %dir.display(), "cannot list recovery copies: {e}"),
        }
    }
    metas.sort_by(|a, b| b.id.cmp(&a.id));
    metas
        .into_iter()
        .filter_map(|m| match store.root.as_ref() {
            // With no root, the server's own dir is all it has, and
            // every copy in it is its.
            None => Some(m),
            Some(_) => store.claimed(m),
        })
        .collect()
}

/// The copy `id` where it is, as this server sees it: `None` when there
/// is none whose file is under its root (or `id` is not one of
/// [`write`]'s, or the meta can't be read).
fn find(store: &Store, id: &str) -> io::Result<Option<(PathBuf, Meta)>> {
    if !valid_id(id) {
        return Ok(None);
    }
    for dir in store.dirs() {
        match read_meta_in(&dir, id) {
            Ok(None) => {}
            Ok(Some(meta)) => {
                let meta = match store.root.as_ref() {
                    None => meta,
                    Some(_) => match store.claimed(meta) {
                        Some(meta) => meta,
                        None => continue,
                    },
                };
                return Ok(Some((dir, meta)));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// The meta of copy `id` in `dir`, or `None` when there is none (or `id`
/// is not one of [`write`]'s).
fn read_meta_in(dir: &Path, id: &str) -> io::Result<Option<Meta>> {
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

/// The meta of copy `id`, as this server sees it, or `None` when there is
/// none (or `id` is not one of [`write`]'s).
pub(crate) fn read_meta(store: &Store, id: &str) -> io::Result<Option<Meta>> {
    Ok(find(store, id)?.map(|(_, meta)| meta))
}

/// `file` with its dir canonical (the file itself may be gone), so every
/// root that holds it maps it to the same place.
fn canonical_file(file: &Path) -> PathBuf {
    match (
        file.parent().and_then(|d| d.canonicalize().ok()),
        file.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => file.to_path_buf(),
    }
}

/// The copies of notebook `path`, newest first, at this server's path
/// for them.
pub(crate) fn list(store: &Store, path: &str) -> io::Result<Vec<Meta>> {
    let mut metas = visible(store);
    // A copy's path comes from its canonical file, so the notebook's own
    // path may spell it differently (a symlinked dir such as macOS's
    // `/tmp`): compare the canonical forms as well.
    let canonical = store.root.as_ref().map(|r| canonical_file(&r.join(path)));
    metas.retain(|m| {
        m.path == path || (canonical.is_some() && m.file.as_deref() == canonical.as_deref())
    });
    for m in &mut metas {
        m.path = path.to_string();
    }
    Ok(metas)
}

/// The copy `id` with its notebook, or `None` when there is none (or `id`
/// is not one of [`write`]'s).
pub(crate) fn read(store: &Store, id: &str) -> io::Result<Option<(Meta, Value)>> {
    let Some((dir, meta)) = find(store, id)? else {
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

/// Delete the copy `id`. Whether there was one this server could see.
pub(crate) fn discard(store: &Store, id: &str) -> io::Result<bool> {
    let Some((dir, _)) = find(store, id)? else {
        return Ok(false);
    };
    discard_in(&dir, id)
}

fn size_of(dir: &Path, meta: &Meta) -> u64 {
    [format!("{}.json", meta.id), meta.notebook.clone()]
        .iter()
        .filter_map(|n| std::fs::metadata(dir.join(n)).ok())
        .map(|m| m.len())
        .sum()
}

/// Delete the oldest copies of a path over [`MAX_PER_PATH`], then the
/// oldest of all while the directory is over `max_bytes`. `keep`, the
/// copy just written, is never deleted: a run whose clock was ahead can
/// leave ids that sort after it, which would make it the oldest of all.
/// Failures are logged: the copy just written is safe either way.
/// Returns the copies deleted.
fn prune(dir: &Path, max_bytes: u64, keep: &str) -> Vec<Meta> {
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
    // Newest first: a path's copies past the limit are its oldest. `keep`
    // still counts toward the limit, but is never deleted, even when the
    // clock having gone back left ids that sort after it and put it past
    // the limit.
    for m in &metas {
        let n = per_path.entry(m.path.as_str()).or_default();
        *n += 1;
        if *n > MAX_PER_PATH && m.id != keep {
            doomed.push(m);
        } else {
            kept.push(m);
        }
    }
    let mut total: u64 = kept.iter().map(|m| size_of(dir, m)).sum();
    // The oldest of the rest, never `keep`, which is left out of the
    // candidates even when it is the oldest of all.
    let mut others: Vec<&Meta> = kept.iter().copied().filter(|m| m.id != keep).collect();
    while total > max_bytes && !others.is_empty() {
        let oldest = others.pop().expect("one more");
        total -= size_of(dir, oldest);
        doomed.push(oldest);
    }
    let mut deleted = Vec::new();
    for m in doomed {
        tracing::warn!(id = %m.id, path = %m.path, "recovery copy over the limit; deleting it");
        match discard_in(dir, &m.id) {
            Ok(_) => deleted.push(m.clone()),
            Err(e) => tracing::warn!(id = %m.id, "cannot delete a recovery copy: {e}"),
        }
    }
    deleted
}

/// Delete the copy `id` from `dir`, where its files are.
fn discard_in(dir: &Path, id: &str) -> io::Result<bool> {
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

    /// A copy with a chosen id, as a run with its clock ahead might have
    /// written it: `write` makes ids of its own.
    fn seeded(dir: &Path, id: &str, path: &str) {
        make_dir(dir).unwrap();
        let meta = Meta {
            id: id.to_string(),
            path: path.to_string(),
            file: None,
            at: "2026-11-25T10:00:00Z".to_string(),
            reason: "save_failed".to_string(),
            detail: String::new(),
            cells: Vec::new(),
            notebook: format!("{id}.ipynb"),
        };
        std::fs::write(dir.join(&meta.notebook), "[]").unwrap();
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();
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
        assert_eq!(list(&one, "a.ipynb").unwrap(), std::slice::from_ref(&mine));
        assert_eq!(list(&two, "a.ipynb").unwrap().len(), MAX_PER_PATH);
        assert!(read(&two, &mine.id).unwrap().is_none());
        assert!(!discard(&two, &mine.id).unwrap());
        assert!(read(&one, &mine.id).unwrap().is_some());
        // The same root by another spelling is the same root.
        let again = Store::new(rec.path(), Some(&roots[0].path().join(".")));
        assert_eq!(again.dir, one.dir);
    }

    /// A copy under a hidden dir of this root is a path the contents API
    /// refuses, so it is not listed, read or discarded here.
    #[test]
    fn a_copy_under_a_hidden_dir_is_not_seen_from_above() {
        let data = tempfile::tempdir().unwrap();
        let outer = tempfile::tempdir().unwrap();
        let outer_path = outer.path().canonicalize().unwrap();
        std::fs::create_dir_all(outer_path.join(".work/proj")).unwrap();
        let rec = data.path().join("recovery");
        make_dir(&rec).unwrap();
        let inner = Store::new(&rec, Some(&outer_path.join(".work/proj")));
        let meta = write(&inner, "a.ipynb", &nb("x"), Reason::SaveFailed, "", &[])
            .unwrap()
            .0;
        assert_eq!(list(&inner, "a.ipynb").unwrap().len(), 1);
        let above = Store::new(&rec, Some(&outer_path));
        assert!(list(&above, ".work/proj/a.ipynb").unwrap().is_empty());
        assert!(read(&above, &meta.id).unwrap().is_none());
        assert!(!discard(&above, &meta.id).unwrap());
        assert_eq!(list(&inner, "a.ipynb").unwrap().len(), 1);
    }

    /// A root reached through a symlinked dir sees the copy at its own
    /// path, as `/tmp` is on macOS.
    #[cfg(unix)]
    #[test]
    fn a_copy_is_found_through_a_symlinked_dir() {
        let data = tempfile::tempdir().unwrap();
        // `outer` holds `real/proj` and `link`, a symlink to `real`, as `/`
        // holds `private/tmp` and `tmp` on macOS.
        let outer = tempfile::tempdir().unwrap();
        let outer_path = outer.path().canonicalize().unwrap();
        std::fs::create_dir_all(outer_path.join("real/proj")).unwrap();
        std::os::unix::fs::symlink(outer_path.join("real"), outer_path.join("link")).unwrap();
        let real = outer_path.join("real");
        let rec = data.path().join("recovery");
        make_dir(&rec).unwrap();
        let inner = Store::new(&rec, Some(&outer_path.join("link/proj")));
        let meta = write(&inner, "a.ipynb", &nb("x"), Reason::SaveFailed, "", &[])
            .unwrap()
            .0;
        let via_link = Store::new(&rec, Some(&outer_path));
        assert!(real.join("proj").is_dir());
        let found = list(&via_link, "link/proj/a.ipynb").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, meta.id);
        assert_eq!(found[0].path, "link/proj/a.ipynb");
    }

    /// A copy is visible from every root that holds its file: one made
    /// by a server rooted at a project is listed, read and discarded by
    /// one rooted at the folder around it, at its path relative to that
    /// root. Roots that hold neither don't see it.
    #[test]
    fn a_copy_is_seen_from_every_root_that_holds_its_file() {
        let rec = tempfile::tempdir().unwrap();
        let outer_root = tempfile::tempdir().unwrap();
        let inner_dir = outer_root.path().join("proj");
        std::fs::create_dir(&inner_dir).unwrap();
        let far = tempfile::tempdir().unwrap();
        let outer = Store::new(rec.path(), Some(outer_root.path()));
        let inner = Store::new(rec.path(), Some(&inner_dir));
        let elsewhere = Store::new(rec.path(), Some(far.path()));
        let mine = write(
            &inner,
            "a.ipynb",
            &nb("mine"),
            Reason::MergeConflict,
            "cell a",
            &[],
        )
        .unwrap()
        .0;
        write(
            &elsewhere,
            "a.ipynb",
            &nb("theirs"),
            Reason::SaveFailed,
            "",
            &[],
        )
        .unwrap();
        // The outer root sees it at the path relative to it; the copy's
        // own path is the inner root's.
        let listed = list(&outer, "proj/a.ipynb").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, mine.id);
        assert_eq!(listed[0].path, "proj/a.ipynb");
        assert_eq!(list(&outer, "a.ipynb").unwrap(), []);
        let (meta, content) = read(&outer, &mine.id).unwrap().unwrap();
        assert_eq!(meta.path, "proj/a.ipynb");
        assert!(content["cells"][0]["source"] == json!(["mine"]));
        // The inner root sees it at its own path, and not the other
        // root's copy; the far root sees only its own.
        let at_inner = list(&inner, "a.ipynb").unwrap();
        assert_eq!(at_inner.len(), 1);
        assert_eq!(at_inner[0].id, mine.id);
        assert_eq!(at_inner[0].path, "a.ipynb");
        assert_eq!(list(&elsewhere, "a.ipynb").unwrap().len(), 1);
        assert_eq!(list(&elsewhere, "proj/a.ipynb").unwrap(), []);
        // Discarding from the outer root removes the copy everywhere.
        assert!(discard(&outer, &mine.id).unwrap());
        assert!(read(&inner, &mine.id).unwrap().is_none());
        assert_eq!(list(&outer, "proj/a.ipynb").unwrap(), []);
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
        let (read_meta, content) = read(&copies, &meta.id).unwrap().unwrap();
        assert_eq!(read_meta, meta);
        assert_eq!(read_meta.reason, "save_failed");
        assert_eq!(content["cells"][0]["source"], json!(["mine"]));
        assert_eq!(
            list(&copies, "d/a.ipynb").unwrap(),
            std::slice::from_ref(&meta)
        );
        assert!(list(&copies, "other.ipynb").unwrap().is_empty());
        assert!(discard(&copies, &meta.id).unwrap());
        assert!(!discard(&copies, &meta.id).unwrap());
        assert!(read(&copies, &meta.id).unwrap().is_none());
        assert_eq!(std::fs::read_dir(&copies.dir).unwrap().count(), 0);
    }

    /// The dirs and copies are private to this user on unix: a copy
    /// holds a notebook.
    #[cfg(unix)]
    #[test]
    fn dirs_and_copies_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let rec = tmp.path().join("recovery");
        make_dir(&rec).unwrap();
        let copies = Store::new(&rec, None);
        let (meta, _) = write(&copies, "a.ipynb", &nb("x"), Reason::SaveFailed, "", &[]).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&rec), 0o700, "the recovery dir");
        assert_eq!(mode(&copies.dir), 0o700, "the root's dir");
        assert_eq!(mode(&copies.dir.join(&meta.notebook)), 0o600);
        assert_eq!(mode(&copies.dir.join(format!("{}.json", meta.id))), 0o600);
    }

    /// The size bound deletes the oldest copies first and keeps the
    /// newest even when it alone is over.
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
        let pruned = prune(&copies.dir, two, "");
        assert_eq!(pruned.len(), 1, "it says what it deleted");
        assert_eq!(pruned[0].id, ids[0]);
        let left: Vec<String> = all(&copies.dir)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left, [ids[2].clone(), ids[1].clone()]);
        let pruned = prune(&copies.dir, 1, &ids[2]);
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].id, ids[1]);
        let left: Vec<String> = all(&copies.dir)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
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
        let left: Vec<String> = list(&copies, "a.ipynb")
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(left.len(), MAX_PER_PATH);
        assert!(!left.contains(&ids[0]), "the oldest is gone");
        assert!(left.contains(&ids[MAX_PER_PATH]), "the newest stays");
        assert!(!copies.dir.join(format!("{}.ipynb", ids[0])).exists());
        assert_eq!(list(&copies, "b.ipynb").unwrap().len(), 1);
    }

    /// The prune after a write never deletes the copy it was called for.
    /// A run whose clock was ahead left copies whose ids sort after every
    /// id this process makes, so the copy just written is the oldest: it
    /// still survives both bounds.
    #[test]
    fn the_prune_after_a_write_keeps_the_copy_just_written() {
        let tmp = tempfile::tempdir().unwrap();
        let copies = store(tmp.path());
        // The path's bound is already met by older copies.
        let future = |i: u32| format!("ffffffffffffffff-{i:08x}");
        for i in 0..u32::try_from(MAX_PER_PATH).unwrap() {
            seeded(&copies.dir, &future(i), "a.ipynb");
        }
        // Counted with the future copies, the new one is the 21st of the
        // path: its own prune must not delete it.
        let (meta, pruned) =
            write(&copies, "a.ipynb", &nb("new"), Reason::SaveFailed, "", &[]).unwrap();
        assert!(pruned.is_empty(), "{pruned:?}");
        assert!(read_meta_in(&copies.dir, &meta.id).unwrap().is_some());
        // The size bound, met only by the others, stops at it too.
        let max = size_of(&copies.dir, &meta) + 1;
        let pruned = prune(&copies.dir, max, &meta.id);
        assert!(pruned.iter().all(|m| m.id != meta.id), "{pruned:?}");
        assert!(read_meta_in(&copies.dir, &meta.id).unwrap().is_some());
    }
}
