//! File ids: an opaque, stable id per file, with the path as a mutable
//! attribute, as `jupyter_server_fileid` keeps them. Document rooms are
//! named by these ids (`json:notebook:<id>`), so a client keeps its room
//! across a rename and a server restart. Nothing is written into the
//! files themselves.
//!
//! With the `sqlite` feature, [`FileIds::open`] keeps them in a SQLite
//! database (its own file, not `jupyter_server_fileid`'s), keyed by the
//! contents root and the API path. Without it, or when the database can't
//! be opened, ids last for the life of the process.
//!
//! Only renames and deletes made through the server move or drop ids
//! ([`FileIds::moved`], [`FileIds::removed`]); a file moved by another
//! program gets a new id when it is next opened.
//!
//! The host API's per-notebook grants live here too, keyed by file id
//! ([`FileIds::grant`], [`FileIds::set_grant`]), so they follow a rename
//! and go with a delete. The store keeps them as opaque JSON text.
//!
//! A grant is kept with a stamp of the file as the server last wrote or
//! saw it (see [`file_stamp`]). A file replaced outside the server keeps
//! its path's id, so [`FileIds::grant_checked`] drops the grant when the
//! file no longer matches its stamp; saves through the server restamp it
//! ([`FileIds::restamp`]). A grant stored without a stamp is stamped the
//! first time it is read (trust on first use).

use crate::path_under as under;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Ids for the files under one contents root.
pub struct FileIds {
    store: Mutex<Store>,
}

struct Store {
    /// Ids for this process: all of them without a database, and with
    /// one, those handed out when it failed, so they still resolve.
    memory: Memory,
    #[cfg(feature = "sqlite")]
    db: Option<(rusqlite::Connection, String)>,
    /// Where the database is.
    #[cfg(feature = "sqlite")]
    db_path: Option<std::path::PathBuf>,
}

#[derive(Default)]
struct Memory {
    by_path: HashMap<String, String>,
    by_id: HashMap<String, String>,
    /// Grants by id, for ids kept in memory.
    grants: HashMap<String, Stored>,
}

/// A grant and the stamp of its file.
struct Stored {
    json: String,
    stamp: Option<String>,
}

/// What a grant's stamp says about its file now.
enum Verdict {
    /// The grant stands as it is.
    Keep,
    /// The grant has no stamp yet: stamp it with the file as it is now.
    Stamp,
    /// The file changed outside the server: the grant goes.
    Drop,
}

/// `stored` is the grant's stamp; `current` the file's now, `None` when
/// it can't be known (the file is missing or can't be read, or there is
/// no local root), which never drops a grant.
fn verdict(stored: Option<&str>, current: Option<&str>) -> Verdict {
    match (stored, current) {
        (_, None) => Verdict::Keep,
        (None, Some(_)) => Verdict::Stamp,
        (Some(a), Some(b)) if a == b => Verdict::Keep,
        (Some(_), Some(_)) => Verdict::Drop,
    }
}

#[cfg(feature = "sqlite")]
/// FNV-1a over `bytes`: a short hash that is the same on every build,
/// unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// `stamp` for the database, tagged with the grant it was taken for. A
/// build from before stamps, sharing the database, can change a grant
/// without touching its stamp; the tag shows the stamp belongs to another
/// grant ([`untag`]).
#[cfg(feature = "sqlite")]
fn tag(json: &str, stamp: &str) -> String {
    format!("{:016x}|{stamp}", fnv1a(json.as_bytes()))
}

/// The file stamp in a stored one, if it was taken for grant `json`;
/// otherwise the grant counts as unstamped.
#[cfg(feature = "sqlite")]
fn untag<'a>(json: &str, stored: Option<&'a str>) -> Option<&'a str> {
    let (hash, stamp) = stored?.split_once('|')?;
    (hash == format!("{:016x}", fnv1a(json.as_bytes()))).then_some(stamp)
}

/// A stamp of the file at `path`, from its metadata, without reading it:
/// its size, modification time and, on Unix, inode number (not the
/// device, which isn't stable across remounts). `None` when it can't be
/// read.
///
/// A file replaced the way `git checkout`, most editors, downloads and
/// sync tools do (a new file renamed over it) has a new inode, and one
/// rewritten in place has a new modification time, so either changes the
/// stamp. The change time is left out on purpose: it moves with `chmod`,
/// extended attributes (Finder, quarantine, iCloud) and links, which are
/// not edits.
///
/// Not seen: a same-size rewrite in place that puts the old modification
/// time back; one within a tick of a filesystem with coarse timestamps
/// (exFAT's 2 s, HFS+'s 1 s); and, on Windows, which has no inode here, a
/// same-size replacement by rename that keeps the modification time.
pub fn file_stamp(path: &Path) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("{}:{mtime}:{}", meta.len(), meta.ino()))
    }
    #[cfg(not(unix))]
    Some(format!("{}:{mtime}", meta.len()))
}

/// [`file_stamp`] of the file at API path `api` under the contents root
/// `root`; `None` for a path that is not a valid API path.
pub fn api_file_stamp(root: &Path, api: &str) -> Option<String> {
    let api = crate::paths::ApiPath::parse(&crate::normalize_api_path(api)).ok()?;
    file_stamp(&api.os_path(root))
}

impl Memory {
    fn index(&mut self, path: &str) -> (String, bool) {
        match self.by_path.get(path) {
            Some(id) => (id.clone(), false),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                self.by_path.insert(path.to_string(), id.clone());
                self.by_id.insert(id.clone(), path.to_string());
                (id, true)
            }
        }
    }

    fn moved(&mut self, old: &str, new: &str) {
        let moving: Vec<(String, String)> = self
            .by_path
            .iter()
            .filter_map(|(p, id)| under(p, old).map(|rest| (format!("{new}{rest}"), id.clone())))
            .collect();
        self.by_path
            .retain(|p, _| under(p, old).is_none() && under(p, new).is_none());
        self.by_id
            .retain(|_, p| under(p, old).is_none() && under(p, new).is_none());
        for (p, id) in moving {
            self.by_path.insert(p.clone(), id.clone());
            self.by_id.insert(id, p);
        }
    }

    fn removed(&mut self, path: &str) {
        self.by_path.retain(|p, _| under(p, path).is_none());
        self.by_id.retain(|_, p| under(p, path).is_none());
        let by_id = &self.by_id;
        self.grants.retain(|id, _| by_id.contains_key(id));
    }
}

impl Default for FileIds {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl std::fmt::Debug for FileIds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileIds").finish_non_exhaustive()
    }
}

impl FileIds {
    /// Ids for this process only.
    pub fn in_memory() -> Self {
        FileIds {
            store: Mutex::new(Store {
                memory: Memory::default(),
                #[cfg(feature = "sqlite")]
                db: None,
                #[cfg(feature = "sqlite")]
                db_path: None,
            }),
        }
    }

    /// Ids kept in the database at `db` for the files under `root`. Falls
    /// back to [`FileIds::in_memory`], with a warning, when the database
    /// can't be opened or the crate was built without `sqlite`. An id
    /// handed out while the database fails is kept in memory, so it
    /// resolves for the life of the process.
    pub fn open(db: &Path, root: &Path) -> Self {
        #[cfg(feature = "sqlite")]
        {
            match sqlite::connect(db) {
                Ok(conn) => {
                    return FileIds {
                        store: Mutex::new(Store {
                            memory: Memory::default(),
                            db: Some((conn, root.to_string_lossy().into_owned())),
                            db_path: Some(db.to_path_buf()),
                        }),
                    }
                }
                Err(e) => tracing::warn!(
                    "file ids are per-process: cannot open {}: {e}",
                    db.display()
                ),
            }
        }
        #[cfg(not(feature = "sqlite"))]
        {
            let _ = root;
            tracing::warn!(
                "file ids are per-process: built without the `sqlite` feature ({})",
                db.display()
            );
        }
        Self::in_memory()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The id for `path`, made the first time; and whether it was new.
    pub fn index(&self, path: &str) -> (String, bool) {
        let store = &mut *self.lock();
        if let Some(id) = store.memory.by_path.get(path) {
            return (id.clone(), false);
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &store.db {
            match sqlite::index(db, root, path) {
                Ok(r) => return r,
                // A path the database already has keeps that id; only a
                // path it lacks gets one in memory.
                Err(e) => match sqlite::id(db, root, path) {
                    Ok(Some(id)) => {
                        tracing::warn!("file id for {path}: {e}");
                        return (id, false);
                    }
                    _ => tracing::warn!("file id for {path}, kept in memory: {e}"),
                },
            }
        }
        store.memory.index(path)
    }

    /// The id `path` has, without making one.
    pub fn get(&self, path: &str) -> Option<String> {
        let store = &*self.lock();
        if let Some(id) = store.memory.by_path.get(path) {
            return Some(id.clone());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &store.db {
            return sqlite::id(db, root, path).unwrap_or_else(|e| {
                tracing::warn!("file id for {path}: {e}");
                None
            });
        }
        None
    }

    /// The path an id names now.
    pub fn path(&self, id: &str) -> Option<String> {
        let store = &*self.lock();
        if let Some(path) = store.memory.by_id.get(id) {
            return Some(path.clone());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &store.db {
            return sqlite::path(db, root, id).unwrap_or_else(|e| {
                tracing::warn!("file id {id}: {e}");
                None
            });
        }
        None
    }

    /// The grant stored for file id `id`: JSON text, as
    /// [`FileIds::set_grant`] stored it, whether or not its file still
    /// matches the stamp. Reads that decide anything use
    /// [`FileIds::grant_checked`].
    pub fn grant(&self, id: &str) -> Option<String> {
        self.lock().grant_json(id)
    }

    /// The grant for file id `id`, if its file still matches. `current`
    /// is the file's stamp now ([`file_stamp`]), or `None` when it can't
    /// be known, which keeps the grant as it is. A grant with no stamp
    /// takes `current` as its own (trust on first use); one whose stamp
    /// differs is deleted, with a warning, and `None` comes back: the
    /// file was changed outside Ganymede.
    pub fn grant_checked(&self, id: &str, current: Option<&str>) -> Option<String> {
        self.grant_checked_with(id, || current.map(str::to_string))
    }

    /// [`FileIds::grant_checked`], with the file's stamp taken by
    /// `current` while the store is held, and only when there is a grant
    /// to check. A save that wrote the file and then restamped it can't
    /// land between the stamp and the comparison: the check sees either
    /// the restamp or no stamp. `current` must not call back into `self`.
    pub fn grant_checked_with(
        &self,
        id: &str,
        current: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        check_locked(&mut self.lock(), id, None, current).grant()
    }

    /// The database these ids are kept in, if one opened: shared with
    /// every other root's ids, which [`held`] looks through.
    pub fn db_path(&self) -> Option<std::path::PathBuf> {
        #[cfg(feature = "sqlite")]
        return self.lock().db_path.clone();
        #[cfg(not(feature = "sqlite"))]
        None
    }

    /// Whether ids and grants outlive this process: kept in a database
    /// that opened.
    pub fn is_persistent(&self) -> bool {
        #[cfg(feature = "sqlite")]
        return self.lock().db.is_some();
        #[cfg(not(feature = "sqlite"))]
        false
    }

    /// Store `grant` for file id `id`, or remove it with `None`. Fails
    /// when the database does, and for an id the database failed to keep
    /// (kept in memory instead), so a grant never silently lasts only
    /// this process when a database is in use.
    ///
    /// `stamp` is the file's [`file_stamp`] as it is now, kept with the
    /// grant; `None` when it can't be had, and the grant is stamped when
    /// it is first read.
    pub fn set_grant(
        &self,
        id: &str,
        grant: Option<&str>,
        stamp: Option<&str>,
    ) -> Result<(), String> {
        self.set_grant_with(id, grant, |_| stamp.map(str::to_string))
    }

    /// [`FileIds::set_grant`], with the stamp taken by `stamp` from the
    /// file's current path while the store is held, so it can't
    /// overwrite a fresher restamp. `stamp` must not call back into
    /// `self`.
    pub fn set_grant_with(
        &self,
        id: &str,
        grant: Option<&str>,
        stamp: impl FnOnce(&str) -> Option<String>,
    ) -> Result<(), String> {
        let store = &mut *self.lock();
        let stamp: Option<String> = grant
            .and_then(|_| store.path_of(id))
            .and_then(|p| stamp(&p));
        let stamp = stamp.as_deref();
        let stored = |g: &str| Stored {
            json: g.to_string(),
            stamp: stamp.map(str::to_string),
        };
        if store.memory.by_id.contains_key(id) {
            #[cfg(feature = "sqlite")]
            if store.db.is_some() {
                return Err(format!(
                    "file id {id} is kept in memory only; the database failed to store it"
                ));
            }
            match grant {
                Some(g) => store.memory.grants.insert(id.to_string(), stored(g)),
                None => store.memory.grants.remove(id),
            };
            return Ok(());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, _)) = &store.db {
            return sqlite::set_grant(db, id, grant, stamp).map_err(|e| e.to_string());
        }
        match grant {
            Some(g) => store.memory.grants.insert(id.to_string(), stored(g)),
            None => store.memory.grants.remove(id),
        };
        Ok(())
    }

    /// Every stored grant under this root: `(path, grant)`, by path,
    /// whether or not its file still matches the stamp. Listings that
    /// show what applies use [`FileIds::grants_checked`].
    pub fn grants(&self) -> Vec<(String, String)> {
        let store = &*self.lock();
        let mut all: Vec<(String, String)> = grant_rows(store)
            .into_iter()
            .filter_map(|(id, path)| Some((path, store.grant_json(&id)?)))
            .collect();
        all.sort();
        all
    }

    /// [`FileIds::grants`], each checked as [`FileIds::grant_checked`]
    /// does, with `current` giving the stamp of the file at an API path
    /// (`None` when it can't be known): a grant whose file changed is
    /// deleted and left out.
    pub fn grants_checked(
        &self,
        current: impl Fn(&str) -> Option<String>,
    ) -> Vec<(String, String)> {
        let store = &mut *self.lock();
        let mut all: Vec<(String, String)> = Vec::new();
        for (id, path) in grant_rows(store) {
            let now = current(&path);
            if let Some(json) = check_locked(store, &id, Some(&path), || now).grant() {
                all.push((path, json));
            }
        }
        all.sort();
        all
    }

    /// Record `stamp` as the file's own for file id `id`, if it has a
    /// grant: a save through the server keeps the grant. Does nothing for
    /// an id with none.
    pub fn restamp(&self, id: &str, stamp: &str) -> Result<(), String> {
        self.restamp_with(id, |_| Some(stamp.to_string()))
    }

    /// [`FileIds::restamp`], with the stamp taken by `stamp` from the
    /// file's current path while the store is held, and only when there
    /// is a grant, so two saves can't leave the older one's stamp. `stamp`
    /// must not call back into `self`.
    pub fn restamp_with(
        &self,
        id: &str,
        stamp: impl FnOnce(&str) -> Option<String>,
    ) -> Result<(), String> {
        let store = &mut *self.lock();
        if store.grant_json(id).is_none() {
            return Ok(());
        }
        let Some(stamp) = store.path_of(id).and_then(|p| stamp(&p)) else {
            return Ok(());
        };
        if let Some(g) = store.memory.grants.get_mut(id) {
            g.stamp = Some(stamp);
            return Ok(());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, _)) = &store.db {
            return sqlite::restamp(db, id, &stamp).map_err(|e| e.to_string());
        }
        Ok(())
    }

    /// The check before a write through the server, for file id `id`:
    /// as [`FileIds::grant_checked_with`], but what comes back is what
    /// [`FileIds::restamp_if`] needs after the write. `None`: no grant
    /// stands (there is none, it was dropped, or it should be and can't be
    /// deleted), so the write must not stamp one. `Some(stamp)`: the grant
    /// stands, stored under `stamp` (`None` when it has none).
    pub fn check_for_write_with(
        &self,
        id: &str,
        current: impl FnOnce() -> Option<String>,
    ) -> Option<Option<String>> {
        match check_locked(&mut self.lock(), id, None, current) {
            Outcome::Grant(_, stamp) => Some(stamp),
            _ => None,
        }
    }

    /// After a write through the server: [`FileIds::restamp_with`], if
    /// the grant stood before the write and is still stored under the
    /// stamp [`FileIds::check_for_write_with`] saw (`seen`). With `None`
    /// (no grant stood), or a grant dropped, replaced or restamped since,
    /// it does nothing.
    pub fn restamp_if(
        &self,
        id: &str,
        seen: &Option<Option<String>>,
        stamp: impl FnOnce(&str) -> Option<String>,
    ) -> Result<(), String> {
        let Some(expected) = seen.as_ref().map(|s| s.as_deref()) else {
            return Ok(());
        };
        let store = &mut *self.lock();
        let path = store.path_of(id);
        let stamp = || path.as_deref().and_then(stamp);
        if let Some(g) = store.memory.grants.get_mut(id) {
            if g.stamp.as_deref() == expected {
                if let Some(stamp) = stamp() {
                    g.stamp = Some(stamp);
                }
            }
            return Ok(());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, _)) = &store.db {
            return sqlite::restamp_if(db, id, expected, stamp).map_err(|e| e.to_string());
        }
        Ok(())
    }

    /// `old` (a file, or a directory and everything under it) is now
    /// `new`; ids follow.
    pub fn moved(&self, old: &str, new: &str) {
        let store = &mut *self.lock();
        store.memory.moved(old, new);
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &mut store.db {
            if let Err(e) = sqlite::moved(db, root, old, new) {
                tracing::warn!("moving file ids from {old} to {new}: {e}");
            }
        }
    }

    /// `path` (a file, or a directory and everything under it) was
    /// deleted.
    pub fn removed(&self, path: &str) {
        let store = &mut *self.lock();
        store.memory.removed(path);
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &store.db {
            if let Err(e) = sqlite::removed(db, root, path) {
                tracing::warn!("dropping file ids under {path}: {e}");
            }
        }
    }
}

/// The contents roots the file ids database at `db` knows `file` under,
/// each with the file's API path there: every directory above `file` is a
/// candidate root, and a root has the file when it has an id for its path
/// in it. `file` is an absolute path whose directory is resolved. A
/// program that writes a file doesn't know which server's root, or
/// `ganymede grant`'s, a grant was made under, so it keeps the grant under
/// every root that has one. Empty when there is no database, or this
/// build has no SQLite.
pub fn roots_with_id(db: &Path, file: &Path) -> Vec<(std::path::PathBuf, String)> {
    #[cfg(feature = "sqlite")]
    {
        if db.is_file() {
            match sqlite::connect(db).and_then(|c| sqlite::roots_with_id(&c, file)) {
                Ok(found) => return found,
                Err(e) => tracing::warn!("looking up file ids for {}: {e}", file.display()),
            }
        }
    }
    let _ = (db, file);
    Vec::new()
}

/// A grant held on a file under one contents root, found by [`held`].
///
/// Grants are per (root, path) in the shared database, and a program that
/// writes a file doesn't know which root, a server's or `ganymede
/// grant`'s, one was made under. It checks every root's grant before
/// writing ([`Held::check`]) and restamps it after ([`Held::restamp`]),
/// with the same compare-and-swap a server's own root gets (#767).
pub struct Held {
    ids: FileIds,
    id: String,
    root: std::path::PathBuf,
    api: String,
    /// What [`Held::check`] saw, for [`Held::restamp`].
    seen: Option<Option<String>>,
}

/// The grants held on `file`, an absolute path, under every contents root
/// above it that has one, except `except` (a root already looked after):
/// one query of the database at `db` by each candidate (root, relative
/// path), then a look at each hit's grant. The file's directory is
/// resolved and its name kept, so a symlinked notebook keeps its own
/// name. Empty when there is no such database.
pub fn held(db: &Path, file: &Path, except: Option<&Path>) -> Vec<Held> {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return Vec::new();
    };
    let Ok(dir) = dir.canonicalize() else {
        return Vec::new();
    };
    roots_with_id(db, &dir.join(name))
        .into_iter()
        .filter(|(root, _)| except != Some(root.as_path()))
        .filter_map(|(root, api)| {
            let ids = FileIds::open(db, &root);
            let id = ids.get(&api)?;
            ids.grant(&id)?;
            Some(Held {
                ids,
                id,
                root,
                api,
                seen: None,
            })
        })
        .collect()
}

impl Held {
    /// The root this grant is under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file's API path under that root.
    pub fn api(&self) -> &str {
        &self.api
    }

    /// Before a write: drops the grant if its file changed outside
    /// Ganymede, and takes note of the stamp it stands under.
    pub fn check(&mut self) {
        let (root, api) = (&self.root, &self.api);
        self.seen = self
            .ids
            .check_for_write_with(&self.id, || api_file_stamp(root, api));
    }

    /// After the write: the grant keeps matching the file, if it stood at
    /// [`Held::check`] and is stored as it was then.
    pub fn restamp(&self) -> Result<(), String> {
        self.ids
            .restamp_if(&self.id, &self.seen, |p| api_file_stamp(&self.root, p))
            .map_err(|e| format!("keeping the grant for {}: {e}", self.api))
    }

    /// After a write that wasn't checked first, such as a copy whose
    /// destination wasn't known: the grant matches the file as it is.
    pub fn restamp_unchecked(&self) -> Result<(), String> {
        self.ids
            .restamp_with(&self.id, |p| api_file_stamp(&self.root, p))
            .map_err(|e| format!("keeping the grant for {}: {e}", self.api))
    }
}

impl Store {
    /// The grant for `id`, as stored.
    fn grant_json(&self, id: &str) -> Option<String> {
        if let Some(g) = self.memory.grants.get(id) {
            return Some(g.json.clone());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, _)) = &self.db {
            return sqlite::grant(db, id).unwrap_or_else(|e| {
                tracing::warn!("grant for file id {id}: {e}");
                None
            });
        }
        None
    }

    /// The path file id `id` names now.
    fn path_of(&self, id: &str) -> Option<String> {
        if let Some(p) = self.memory.by_id.get(id) {
            return Some(p.clone());
        }
        #[cfg(feature = "sqlite")]
        if let Some((db, root)) = &self.db {
            return sqlite::path(db, root, id).ok().flatten();
        }
        None
    }

    /// The path file id `id` names now, for a log line.
    fn path_for_log(&self, id: &str) -> String {
        self.path_of(id).unwrap_or_else(|| format!("file id {id}"))
    }
}

/// `(id, path)` of every file under this root with a grant.
fn grant_rows(store: &Store) -> Vec<(String, String)> {
    #[cfg_attr(not(feature = "sqlite"), allow(unused_mut))]
    let mut rows: Vec<(String, String)> = store
        .memory
        .grants
        .keys()
        .filter_map(|id| Some((id.clone(), store.memory.by_id.get(id)?.clone())))
        .collect();
    #[cfg(feature = "sqlite")]
    if let Some((db, root)) = &store.db {
        match sqlite::grant_rows(db, root) {
            Ok(r) => rows.extend(r),
            Err(e) => tracing::warn!("listing grants: {e}"),
        }
    }
    rows
}

/// What a check found.
enum Outcome {
    /// No grant.
    Missing,
    /// The grant stands, with the file stamp it is stored under now.
    Grant(String, Option<String>),
    /// The file changed and the grant doesn't stand: it is gone, or it
    /// should be and the database can't delete it.
    Dropped,
}

impl Outcome {
    fn grant(self) -> Option<String> {
        match self {
            Outcome::Grant(json, _) => Some(json),
            _ => None,
        }
    }
}

/// The grant for `id` if its file matches: see [`FileIds::grant_checked`].
/// `path` is the file's path when the caller has it, for the log.
fn check_locked(
    store: &mut Store,
    id: &str,
    path: Option<&str>,
    current: impl FnOnce() -> Option<String>,
) -> Outcome {
    let dropped = |store: &Store| {
        let path = path.map_or_else(|| store.path_for_log(id), str::to_string);
        tracing::warn!("dropped the host grant for {path}: the file changed outside Ganymede");
    };
    if let Some(g) = store.memory.grants.get_mut(id) {
        let json = g.json.clone();
        let current = current();
        match verdict(g.stamp.as_deref(), current.as_deref()) {
            Verdict::Keep => return Outcome::Grant(json, g.stamp.clone()),
            Verdict::Stamp => {
                g.stamp = current.clone();
                return Outcome::Grant(json, current);
            }
            Verdict::Drop => {
                store.memory.grants.remove(id);
                dropped(store);
                return Outcome::Dropped;
            }
        }
    }
    #[cfg(feature = "sqlite")]
    if let Some((db, _)) = &store.db {
        return match sqlite::check(db, id, current) {
            Ok(sqlite::Checked::Missing) => Outcome::Missing,
            Ok(sqlite::Checked::Grant(json, stamp)) => Outcome::Grant(json, stamp),
            Ok(sqlite::Checked::Dropped) => {
                dropped(store);
                Outcome::Dropped
            }
            // The file changed and the grant can't be deleted: it isn't
            // handed out.
            Ok(sqlite::Checked::Stuck(e)) => {
                tracing::warn!("dropping the grant for file id {id}: {e}");
                Outcome::Dropped
            }
            // A read that failed keeps the grant, as an unreadable file
            // does.
            Err(e) => {
                tracing::warn!("checking the grant for file id {id}: {e}");
                match store.grant_json(id) {
                    Some(json) => Outcome::Grant(json, None),
                    None => Outcome::Missing,
                }
            }
        };
    }
    Outcome::Missing
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use rusqlite::{params, Connection, OptionalExtension};
    use std::path::Path;
    use std::time::Duration;

    pub fn connect(path: &Path) -> rusqlite::Result<Connection> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let db = Connection::open(path)?;
        // Several servers (one per root) may share the file.
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_ids
            (
                id text PRIMARY KEY,
                root text NOT NULL,
                path text NOT NULL,
                UNIQUE (root, path)
            );
            CREATE TABLE IF NOT EXISTS host_grants
            (
                id text PRIMARY KEY,
                grant_json text NOT NULL,
                stamp text
            );",
        )?;
        add_stamp_column(&db)?;
        Ok(db)
    }

    /// A database from before grants had stamps lacks the column. Other
    /// servers share the file, so one may add it between the check and
    /// the `ALTER`: that is not an error.
    fn add_stamp_column(db: &Connection) -> rusqlite::Result<()> {
        let has = db
            .prepare("SELECT name FROM pragma_table_info('host_grants')")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|n| n == "stamp");
        if has {
            return Ok(());
        }
        match db.execute("ALTER TABLE host_grants ADD COLUMN stamp text", []) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("duplicate column") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Another server on the same root may insert the path first: the
    /// insert then does nothing, and its id is the one read back.
    pub fn index(db: &Connection, root: &str, path: &str) -> rusqlite::Result<(String, bool)> {
        // Most paths have an id already: read it without taking the
        // write lock.
        if let Some(id) = self::id(db, root, path)? {
            return Ok((id, false));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let inserted = db.execute(
            "INSERT OR IGNORE INTO file_ids (id, root, path) VALUES (?1, ?2, ?3)",
            params![id, root, path],
        )?;
        let stored: String = db.query_row(
            "SELECT id FROM file_ids WHERE root = ?1 AND path = ?2",
            params![root, path],
            |r| r.get(0),
        )?;
        Ok((stored, inserted == 1))
    }

    pub fn id(db: &Connection, root: &str, path: &str) -> rusqlite::Result<Option<String>> {
        db.query_row(
            "SELECT id FROM file_ids WHERE root = ?1 AND path = ?2",
            params![root, path],
            |r| r.get(0),
        )
        .optional()
    }

    pub fn path(db: &Connection, root: &str, id: &str) -> rusqlite::Result<Option<String>> {
        db.query_row(
            "SELECT path FROM file_ids WHERE root = ?1 AND id = ?2",
            params![root, id],
            |r| r.get(0),
        )
        .optional()
    }

    /// Rows for `path` and everything under it.
    const UNDER: &str =
        "root = ?1 AND (path = ?2 OR ?2 = '' OR substr(path, 1, length(?2) + 1) = ?2 || '/')";

    pub fn moved(db: &mut Connection, root: &str, old: &str, new: &str) -> rusqlite::Result<()> {
        let tx = db.transaction()?;
        // Anything already at the destination is gone (renames don't
        // replace, but a file may have been deleted outside the server).
        tx.execute(
            &format!("DELETE FROM file_ids WHERE {UNDER}"),
            params![root, new],
        )?;
        tx.execute(
            &format!("UPDATE file_ids SET path = ?3 || substr(path, length(?2) + 1) WHERE {UNDER}"),
            params![root, old, new],
        )?;
        tx.commit()
    }

    pub fn removed(db: &Connection, root: &str, path: &str) -> rusqlite::Result<()> {
        db.execute(
            &format!("DELETE FROM host_grants WHERE id IN (SELECT id FROM file_ids WHERE {UNDER})"),
            params![root, path],
        )?;
        db.execute(
            &format!("DELETE FROM file_ids WHERE {UNDER}"),
            params![root, path],
        )?;
        Ok(())
    }

    pub fn grant(db: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
        db.query_row(
            "SELECT grant_json FROM host_grants WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()
    }

    pub fn set_grant(
        db: &Connection,
        id: &str,
        grant: Option<&str>,
        stamp: Option<&str>,
    ) -> rusqlite::Result<()> {
        match grant {
            Some(g) => db.execute(
                "INSERT INTO host_grants (id, grant_json, stamp) VALUES (?1, ?2, ?3)
                ON CONFLICT (id) DO UPDATE SET
                    grant_json = excluded.grant_json, stamp = excluded.stamp",
                params![id, g, stamp.map(|s| super::tag(g, s))],
            )?,
            None => db.execute("DELETE FROM host_grants WHERE id = ?1", params![id])?,
        };
        Ok(())
    }

    pub fn restamp(db: &Connection, id: &str, stamp: &str) -> rusqlite::Result<()> {
        let Some(json) = grant(db, id)? else {
            return Ok(());
        };
        // Only for the grant just read: another server may have changed it.
        db.execute(
            "UPDATE host_grants SET stamp = ?2 WHERE id = ?1 AND grant_json = ?3",
            params![id, super::tag(&json, stamp), json],
        )?;
        Ok(())
    }

    pub enum Checked {
        Missing,
        /// The grant and the file stamp stored under it.
        Grant(String, Option<String>),
        Dropped,
        /// The grant should go, and the database wouldn't delete it.
        Stuck(rusqlite::Error),
    }

    /// As [`super::FileIds::grant_checked`], in the database. Another
    /// server may restamp or replace the grant meanwhile, so the stamp
    /// and the delete go by the row that was read, and a changed row is
    /// read again. An `Err` is a failed read or stamp: the grant stands.
    pub fn check(
        db: &Connection,
        id: &str,
        current: impl FnOnce() -> Option<String>,
    ) -> rusqlite::Result<Checked> {
        let row: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT grant_json, stamp FROM host_grants WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((json, raw)) = row else {
            return Ok(Checked::Missing);
        };
        let current = current();
        let current = current.as_deref();
        match super::verdict(super::untag(&json, raw.as_deref()), current) {
            super::Verdict::Keep => {
                let stamp = super::untag(&json, raw.as_deref()).map(str::to_string);
                Ok(Checked::Grant(json, stamp))
            }
            super::Verdict::Stamp => {
                let tagged = current.map(|c| super::tag(&json, c));
                db.execute(
                    "UPDATE host_grants SET stamp = ?2 WHERE id = ?1 AND grant_json = ?3
                        AND stamp IS ?4",
                    params![id, tagged, json, raw],
                )?;
                Ok(Checked::Grant(json, current.map(str::to_string)))
            }
            super::Verdict::Drop => {
                let gone = match db.execute(
                    "DELETE FROM host_grants WHERE id = ?1 AND grant_json = ?2 AND stamp = ?3",
                    params![id, json, raw],
                ) {
                    Ok(n) => n,
                    Err(e) => return Ok(Checked::Stuck(e)),
                };
                if gone == 1 {
                    return Ok(Checked::Dropped);
                }
                Ok(match grant_row(db, id)? {
                    Some((json, stamp)) => Checked::Grant(json, stamp),
                    None => Checked::Missing,
                })
            }
        }
    }

    /// The grant and the file stamp stored under it, if for that grant.
    pub fn grant_row(
        db: &Connection,
        id: &str,
    ) -> rusqlite::Result<Option<(String, Option<String>)>> {
        let row: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT grant_json, stamp FROM host_grants WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(json, raw)| {
            let stamp = super::untag(&json, raw.as_deref()).map(str::to_string);
            (json, stamp)
        }))
    }

    /// As [`super::FileIds::restamp_if`], in the database.
    pub fn restamp_if(
        db: &Connection,
        id: &str,
        expected: Option<&str>,
        stamp: impl FnOnce() -> Option<String>,
    ) -> rusqlite::Result<()> {
        let row: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT grant_json, stamp FROM host_grants WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((json, raw)) = row else {
            return Ok(());
        };
        if super::untag(&json, raw.as_deref()) != expected {
            return Ok(());
        }
        let Some(stamp) = stamp() else {
            return Ok(());
        };
        // Only the row just read: another server may have changed it.
        db.execute(
            "UPDATE host_grants SET stamp = ?2 WHERE id = ?1 AND grant_json = ?3 AND stamp IS ?4",
            params![id, super::tag(&json, &stamp), json, raw],
        )?;
        Ok(())
    }

    pub fn roots_with_id(
        db: &Connection,
        file: &Path,
    ) -> rusqlite::Result<Vec<(std::path::PathBuf, String)>> {
        let mut candidates: Vec<(String, String)> = Vec::new();
        for root in file.ancestors().skip(1) {
            let (Some(r), Ok(rel)) = (root.to_str(), file.strip_prefix(root)) else {
                continue;
            };
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            if let Some(parts) = parts {
                candidates.push((r.to_string(), parts.join("/")));
            }
        }
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let clause = vec!["(root = ? AND path = ?)"; candidates.len()].join(" OR ");
        let mut q = db.prepare(&format!("SELECT root, path FROM file_ids WHERE {clause}"))?;
        let args = candidates.iter().flat_map(|(r, p)| [r, p]);
        let rows = q.query_map(rusqlite::params_from_iter(args), |r| {
            Ok((std::path::PathBuf::from(r.get::<_, String>(0)?), r.get(1)?))
        })?;
        rows.collect()
    }

    /// `(id, path)` of every grant under `root`.
    pub fn grant_rows(db: &Connection, root: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let mut q = db.prepare(
            "SELECT g.id, f.path FROM host_grants g JOIN file_ids f ON f.id = g.id
            WHERE f.root = ?1",
        )?;
        let rows = q.query_map(params![root], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(ids: &FileIds) {
        let (a, new) = ids.index("dir/a.ipynb");
        assert!(new);
        assert_eq!(ids.index("dir/a.ipynb"), (a.clone(), false));
        let (b, _) = ids.index("dir/sub/b.ipynb");
        let (c, _) = ids.index("dirt.ipynb");
        assert_eq!(ids.path(&a).as_deref(), Some("dir/a.ipynb"));
        assert_eq!(ids.get("dir/a.ipynb"), Some(a.clone()));
        assert_eq!(ids.get("nope"), None);

        ids.moved("dir", "renamed");
        assert_eq!(ids.path(&a).as_deref(), Some("renamed/a.ipynb"));
        assert_eq!(ids.path(&b).as_deref(), Some("renamed/sub/b.ipynb"));
        assert_eq!(
            ids.path(&c).as_deref(),
            Some("dirt.ipynb"),
            "a sibling with the prefix stays"
        );
        assert_eq!(ids.index("renamed/a.ipynb"), (a.clone(), false));

        ids.moved("renamed/a.ipynb", "a.ipynb");
        assert_eq!(ids.path(&a).as_deref(), Some("a.ipynb"));

        ids.removed("renamed");
        assert_eq!(ids.path(&b), None);
        assert_eq!(ids.path(&a).as_deref(), Some("a.ipynb"));
        let (b2, new) = ids.index("renamed/sub/b.ipynb");
        assert!(new);
        assert_ne!(b2, b);
    }

    #[test]
    fn in_memory_ids_follow_moves() {
        exercise(&FileIds::in_memory());
        grants_follow_moves(&FileIds::in_memory());
    }

    fn grants_follow_moves(ids: &FileIds) {
        let (a, _) = ids.index("dir/a.ipynb");
        let (b, _) = ids.index("b.ipynb");
        ids.set_grant(&a, Some(r#"{"run":[""]}"#), None).unwrap();
        ids.set_grant(&b, Some("{}"), None).unwrap();
        ids.set_grant(&b, Some(r#"{"edit_self":false}"#), None)
            .unwrap();
        assert_eq!(ids.grant(&a).as_deref(), Some(r#"{"run":[""]}"#));
        ids.moved("dir", "moved");
        assert_eq!(
            ids.grants(),
            [
                ("b.ipynb".to_string(), r#"{"edit_self":false}"#.to_string()),
                ("moved/a.ipynb".to_string(), r#"{"run":[""]}"#.to_string()),
            ]
        );
        ids.removed("moved");
        assert_eq!(ids.grant(&a), None);
        ids.set_grant(&b, None, None).unwrap();
        assert_eq!(ids.grant(&b), None);
        assert!(ids.grants().is_empty());
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_ids_follow_moves_and_persist_per_root() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        exercise(&FileIds::open(&db, Path::new("/root1")));

        let first = FileIds::open(&db, Path::new("/root1"));
        let (id, new) = first.index("x.ipynb");
        assert!(new);
        drop(first);
        let again = FileIds::open(&db, Path::new("/root1"));
        assert_eq!(again.index("x.ipynb"), (id.clone(), false));
        let other = FileIds::open(&db, Path::new("/root2"));
        assert_eq!(other.path(&id), None);
        assert!(other.index("x.ipynb").1);

        grants_follow_moves(&FileIds::open(&db, Path::new("/root3")));
        // Grants persist, and list only under their own root.
        let ids = FileIds::open(&db, Path::new("/root1"));
        ids.set_grant(&id, Some("{}"), None).unwrap();
        drop(ids);
        assert_eq!(
            FileIds::open(&db, Path::new("/root1"))
                .grant(&id)
                .as_deref(),
            Some("{}")
        );
        assert!(other.grants().is_empty());
    }

    /// Two servers on one root index the same new path: both get the id
    /// the first stored.
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_index_takes_the_stored_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let a = sqlite::connect(&db).unwrap();
        let b = sqlite::connect(&db).unwrap();
        let (id, new) = sqlite::index(&a, "/r", "x.ipynb").unwrap();
        assert!(new);
        assert_eq!(sqlite::index(&b, "/r", "x.ipynb").unwrap(), (id, false));
    }

    /// When the database fails, an id handed out still resolves, and
    /// follows moves and deletes.
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_errors_keep_ids_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let ids = FileIds::open(&db, Path::new("/root"));
        let (stored, _) = ids.index("stored.ipynb");
        sqlite::connect(&db)
            .unwrap()
            .execute_batch("DROP TABLE file_ids")
            .unwrap();
        let (id, new) = ids.index("a.ipynb");
        assert!(new);
        assert_eq!(ids.index("a.ipynb"), (id.clone(), false));
        assert_eq!(ids.path(&id).as_deref(), Some("a.ipynb"));
        assert_eq!(ids.get("a.ipynb"), Some(id.clone()));
        // A grant on an id the database didn't keep would last only this
        // process: refused.
        assert!(ids.set_grant(&id, Some("{}"), None).is_err());
        assert!(ids.is_persistent());
        ids.moved("a.ipynb", "b.ipynb");
        assert_eq!(ids.path(&id).as_deref(), Some("b.ipynb"));
        ids.removed("b.ipynb");
        assert_eq!(ids.path(&id), None);
        // What the database held is out of reach while it fails.
        assert_eq!(ids.path(&stored), None);
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn a_failed_write_keeps_the_stored_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let ids = FileIds::open(&db, Path::new("/root"));
        let (stored, _) = ids.index("stored.ipynb");
        // Writes fail (as when another server holds the lock past the
        // busy timeout); reads still work.
        sqlite::connect(&db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER no_insert BEFORE INSERT ON file_ids
                BEGIN SELECT RAISE(FAIL, 'busy'); END;",
            )
            .unwrap();
        let fresh = FileIds::open(&db, Path::new("/root"));
        assert_eq!(fresh.index("stored.ipynb"), (stored.clone(), false));
        let (id, new) = fresh.index("a.ipynb");
        assert!(new);
        assert_ne!(id, stored);
        assert_eq!(fresh.index("a.ipynb"), (id, false));
        assert_eq!(fresh.index("stored.ipynb"), (stored, false));
    }

    const G: &str = r#"{"run":[""]}"#;

    /// The stamp rules, on either store.
    fn stamps(ids: &FileIds) {
        let (a, _) = ids.index("a.ipynb");
        let (b, _) = ids.index("b.ipynb");
        ids.set_grant(&a, Some(G), Some("s1")).unwrap();
        // The same stamp, or none to compare (a missing file), keeps it.
        assert_eq!(ids.grant_checked(&a, Some("s1")).as_deref(), Some(G));
        assert_eq!(ids.grant_checked(&a, None).as_deref(), Some(G));
        // Restamped: the new stamp is the file's own.
        ids.restamp(&a, "s2").unwrap();
        assert_eq!(ids.grant_checked(&a, Some("s2")).as_deref(), Some(G));
        // A restamp for an id with no grant stores nothing.
        ids.restamp(&b, "x").unwrap();
        assert_eq!(ids.grant(&b), None);
        ids.set_grant(&b, Some("{}"), None).unwrap();
        assert_eq!(ids.grant_checked(&b, Some("x")).as_deref(), Some("{}"));
        assert_eq!(ids.grant_checked(&b, Some("x")).as_deref(), Some("{}"));
        // A different stamp drops it: for good, and from the listing.
        assert_eq!(ids.grant_checked(&a, Some("other")), None);
        assert_eq!(ids.grant(&a), None);
        assert_eq!(ids.grants(), [("b.ipynb".to_string(), "{}".to_string())]);
        // A grant stamped on first read drops when its file then changes.
        assert_eq!(ids.grant_checked(&b, Some("y")), None);
        assert!(ids.grants().is_empty());
        // The listing checks each grant by its path.
        ids.set_grant(&a, Some(G), Some("s1")).unwrap();
        ids.set_grant(&b, Some("{}"), Some("s1")).unwrap();
        let live = ids.grants_checked(|p| Some(if p == "a.ipynb" { "s1" } else { "s9" }.into()));
        assert_eq!(live, [("a.ipynb".to_string(), G.to_string())]);
        assert_eq!(ids.grant(&b), None);
        // An unstamped grant is stamped by the listing too.
        ids.set_grant(&b, Some("{}"), None).unwrap();
        let stamped = |p: &str| Some(if p == "a.ipynb" { "s1" } else { "t" }.to_string());
        assert_eq!(ids.grants_checked(stamped).len(), 2);
        assert_eq!(ids.grant_checked(&b, Some("u")), None);
    }

    #[test]
    fn in_memory_grants_follow_their_stamps() {
        stamps(&FileIds::in_memory());
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_grants_follow_their_stamps() {
        let dir = tempfile::tempdir().unwrap();
        stamps(&FileIds::open(&dir.path().join("ids.db"), Path::new("/r")));
    }

    /// A database from before stamps: the column is added, and the old
    /// grant is stamped by its first read, not dropped. Opening twice
    /// (two servers) is fine.
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_without_the_stamp_column_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        {
            let old = rusqlite::Connection::open(&db).unwrap();
            old.execute_batch(
                "CREATE TABLE file_ids (id text PRIMARY KEY, root text NOT NULL,
                    path text NOT NULL, UNIQUE (root, path));
                CREATE TABLE host_grants (id text PRIMARY KEY, grant_json text NOT NULL);
                INSERT INTO file_ids VALUES ('i1', '/r', 'a.ipynb');
                INSERT INTO host_grants VALUES ('i1', '{\"run\":[\"\"]}');",
            )
            .unwrap();
        }
        let ids = FileIds::open(&db, Path::new("/r"));
        let again = FileIds::open(&db, Path::new("/r"));
        assert!(ids.is_persistent() && again.is_persistent());
        assert_eq!(ids.grant_checked("i1", Some("s1")).as_deref(), Some(G));
        assert_eq!(again.grant_checked("i1", Some("s1")).as_deref(), Some(G));
        assert_eq!(again.grant_checked("i1", Some("s2")), None);
        assert!(ids.grants().is_empty());
    }

    #[test]
    fn a_stamp_changes_with_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.ipynb");
        assert_eq!(file_stamp(&f), None);
        assert_eq!(api_file_stamp(dir.path(), "a.ipynb"), None);
        std::fs::write(&f, "1234").unwrap();
        let first = file_stamp(&f).unwrap();
        assert_eq!(api_file_stamp(dir.path(), "/a.ipynb"), Some(first.clone()));
        assert_eq!(api_file_stamp(dir.path(), "../a.ipynb"), None);
        assert_eq!(file_stamp(&f).unwrap(), first);
        std::fs::write(&f, "12345").unwrap();
        assert_ne!(file_stamp(&f).unwrap(), first);
    }

    /// Replaced by a rename, as `git checkout` and editors do, with the
    /// same size and the old modification time put back: the new inode
    /// shows. A rewrite in place that does the same is not seen; and
    /// `chmod` is no change.
    #[cfg(unix)]
    #[test]
    fn a_stamp_sees_a_replaced_file_not_a_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.ipynb");
        std::fs::write(&f, "aaaa").unwrap();
        let first = file_stamp(&f).unwrap();
        let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();

        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(file_stamp(&f).unwrap(), first, "chmod isn't an edit");

        let new = dir.path().join("new");
        std::fs::write(&new, "bbbb").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&new)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        std::fs::rename(&new, &f).unwrap();
        assert_ne!(file_stamp(&f).unwrap(), first);

        // In place, same size, mtime restored: the known blind spot.
        let second = file_stamp(&f).unwrap();
        std::fs::write(&f, "cccc").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(file_stamp(&f).unwrap(), second);
    }

    /// A build from before stamps changes a grant and leaves its stamp:
    /// the stamp isn't the new grant's, so the grant is stamped afresh,
    /// not dropped.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_stamp_taken_for_another_grant_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let ids = FileIds::open(&db, Path::new("/r"));
        let (id, _) = ids.index("a.ipynb");
        ids.set_grant(&id, Some(G), Some("s1")).unwrap();
        assert_eq!(ids.grant_checked(&id, Some("s1")).as_deref(), Some(G));
        sqlite::connect(&db)
            .unwrap()
            .execute("UPDATE host_grants SET grant_json = '{}'", [])
            .unwrap();
        assert_eq!(ids.grant_checked(&id, Some("s2")).as_deref(), Some("{}"));
        // Now it is stamped, for this grant.
        assert_eq!(ids.grant_checked(&id, Some("s3")), None);
        // A restamp is for the grant stored when it is made.
        ids.set_grant(&id, Some(G), Some("s1")).unwrap();
        ids.restamp(&id, "s4").unwrap();
        assert_eq!(ids.grant_checked(&id, Some("s4")).as_deref(), Some(G));
    }

    /// When the file changed and the database won't delete the grant, the
    /// grant isn't handed out; when it only can't be read, it stands.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_grant_that_cannot_be_deleted_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let ids = FileIds::open(&db, Path::new("/r"));
        let (id, _) = ids.index("a.ipynb");
        ids.set_grant(&id, Some(G), Some("s1")).unwrap();
        sqlite::connect(&db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER no_delete BEFORE DELETE ON host_grants
                BEGIN SELECT RAISE(FAIL, 'busy'); END;",
            )
            .unwrap();
        assert_eq!(ids.grant_checked(&id, Some("s1")).as_deref(), Some(G));
        assert_eq!(ids.grant_checked(&id, Some("other")), None);
        assert!(ids.grants_checked(|_| Some("other".into())).is_empty());
        // Not deleted, so a file that matches again has its grant.
        assert_eq!(ids.grant_checked(&id, Some("s1")).as_deref(), Some(G));
    }

    /// The stamp is taken with the store held, so a check and a restamp
    /// can't interleave with another thread's.
    #[test]
    fn stamps_are_taken_with_the_store_held() {
        let ids = FileIds::in_memory();
        let (a, _) = ids.index("a.ipynb");
        let held = || ids.store.try_lock().is_err();
        ids.set_grant_with(&a, Some(G), |p| {
            assert_eq!(p, "a.ipynb");
            assert!(held());
            Some("s1".into())
        })
        .unwrap();
        assert_eq!(
            ids.grant_checked_with(&a, || {
                assert!(held());
                Some("s1".into())
            })
            .as_deref(),
            Some(G)
        );
        ids.restamp_with(&a, |_| {
            assert!(held());
            Some("s2".into())
        })
        .unwrap();
        assert_eq!(ids.grant_checked(&a, Some("s2")).as_deref(), Some(G));
        // Nothing is stat'ed for a file with no grant.
        let (b, _) = ids.index("b.ipynb");
        assert_eq!(ids.grant_checked_with(&b, || panic!("no grant")), None);
        ids.restamp_with(&b, |_| panic!("no grant")).unwrap();
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn roots_with_id_finds_the_roots_above_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        assert!(roots_with_id(&db, Path::new("/p/s/nb.ipynb")).is_empty());
        let outer = FileIds::open(&db, Path::new("/p"));
        let inner = FileIds::open(&db, Path::new("/p/s"));
        let unrelated = FileIds::open(&db, Path::new("/q"));
        outer.index("s/nb.ipynb");
        inner.index("nb.ipynb");
        unrelated.index("s/nb.ipynb");
        let mut found = roots_with_id(&db, Path::new("/p/s/nb.ipynb"));
        found.sort();
        assert_eq!(
            found,
            [
                (std::path::PathBuf::from("/p"), "s/nb.ipynb".to_string()),
                (std::path::PathBuf::from("/p/s"), "nb.ipynb".to_string()),
            ]
        );
    }

    /// A restamp after a write goes by what the check before it saw.
    fn restamps_compare(ids: &FileIds) {
        let (a, _) = ids.index("a.ipynb");
        let (b, _) = ids.index("b.ipynb");
        // No grant: nothing to stamp.
        assert_eq!(ids.check_for_write_with(&b, || Some("x".into())), None);
        ids.set_grant(&a, Some(G), Some("s1")).unwrap();
        let seen = ids.check_for_write_with(&a, || Some("s1".into()));
        assert_eq!(seen, Some(Some("s1".to_string())));
        // Another writer restamped meanwhile: left as it is.
        ids.restamp(&a, "s2").unwrap();
        ids.restamp_if(&a, &seen, |_| Some("s3".into())).unwrap();
        assert_eq!(ids.grant_checked(&a, Some("s2")).as_deref(), Some(G));
        // As it was seen: stamped.
        ids.restamp_if(&a, &Some(Some("s2".into())), |_| Some("s3".into()))
            .unwrap();
        assert_eq!(ids.grant_checked(&a, Some("s3")).as_deref(), Some(G));
        // A dropped grant isn't brought back, and one with no stamp takes
        // the file's.
        assert_eq!(ids.check_for_write_with(&a, || Some("other".into())), None);
        ids.set_grant(&b, Some(G), None).unwrap();
        assert_eq!(ids.check_for_write_with(&b, || None), Some(None));
        ids.restamp_if(&b, &Some(None), |_| Some("t1".into()))
            .unwrap();
        assert_eq!(ids.grant_checked(&b, Some("t1")).as_deref(), Some(G));
    }

    #[test]
    fn in_memory_restamps_compare() {
        restamps_compare(&FileIds::in_memory());
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_restamps_compare() {
        let dir = tempfile::tempdir().unwrap();
        restamps_compare(&FileIds::open(&dir.path().join("ids.db"), Path::new("/r")));
    }

    /// The database won't delete a grant its file outgrew: the write
    /// that follows has nothing to restamp, and once the database works
    /// again the grant doesn't match the new file.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_stuck_grant_is_not_restamped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ids.db");
        let ids = FileIds::open(&db, Path::new("/r"));
        let (id, _) = ids.index("a.ipynb");
        ids.set_grant(&id, Some(G), Some("s1")).unwrap();
        let other = sqlite::connect(&db).unwrap();
        other
            .execute_batch(
                "CREATE TRIGGER no_delete BEFORE DELETE ON host_grants
                BEGIN SELECT RAISE(FAIL, 'busy'); END;",
            )
            .unwrap();
        let seen = ids.check_for_write_with(&id, || Some("outside".into()));
        assert_eq!(seen, None);
        other.execute_batch("DROP TRIGGER no_delete").unwrap();
        // The write happens, and its restamp, as callers make it with
        // what the check returned: nothing is stamped, so the grant, still
        // stored for the old file, doesn't match the new one.
        ids.restamp_if(&id, &seen, |_| Some("written".into()))
            .unwrap();
        assert_eq!(ids.grant_checked(&id, Some("written")), None);
    }
}
