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

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Ids for the files under one contents root.
pub struct FileIds {
    store: Mutex<Store>,
}

enum Store {
    Memory {
        by_path: HashMap<String, String>,
        by_id: HashMap<String, String>,
    },
    #[cfg(feature = "sqlite")]
    Sqlite {
        db: rusqlite::Connection,
        root: String,
    },
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

/// `old` itself or something under it, as `(rest)` after `old`.
fn under<'a>(path: &'a str, old: &str) -> Option<&'a str> {
    if path == old {
        return Some("");
    }
    if old.is_empty() {
        return Some(path);
    }
    path.strip_prefix(old)?
        .strip_prefix('/')
        .map(|_| &path[old.len()..])
}

impl FileIds {
    /// Ids for this process only.
    pub fn in_memory() -> Self {
        FileIds {
            store: Mutex::new(Store::Memory {
                by_path: HashMap::new(),
                by_id: HashMap::new(),
            }),
        }
    }

    /// Ids kept in the database at `db` for the files under `root`. Falls
    /// back to [`FileIds::in_memory`], with a warning, when the database
    /// can't be opened or the crate was built without `sqlite`.
    pub fn open(db: &Path, root: &Path) -> Self {
        #[cfg(feature = "sqlite")]
        {
            match sqlite::connect(db) {
                Ok(conn) => {
                    return FileIds {
                        store: Mutex::new(Store::Sqlite {
                            db: conn,
                            root: root.to_string_lossy().into_owned(),
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
        match &mut *self.lock() {
            Store::Memory { by_path, by_id } => match by_path.get(path) {
                Some(id) => (id.clone(), false),
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    by_path.insert(path.to_string(), id.clone());
                    by_id.insert(id.clone(), path.to_string());
                    (id, true)
                }
            },
            #[cfg(feature = "sqlite")]
            Store::Sqlite { db, root } => sqlite::index(db, root, path).unwrap_or_else(|e| {
                tracing::warn!("file id for {path}: {e}");
                (uuid::Uuid::new_v4().to_string(), true)
            }),
        }
    }

    /// The path an id names now.
    pub fn path(&self, id: &str) -> Option<String> {
        match &*self.lock() {
            Store::Memory { by_id, .. } => by_id.get(id).cloned(),
            #[cfg(feature = "sqlite")]
            Store::Sqlite { db, root } => sqlite::path(db, root, id).unwrap_or_else(|e| {
                tracing::warn!("file id {id}: {e}");
                None
            }),
        }
    }

    /// `old` (a file, or a directory and everything under it) is now
    /// `new`; ids follow.
    pub fn moved(&self, old: &str, new: &str) {
        match &mut *self.lock() {
            Store::Memory { by_path, by_id } => {
                let moving: Vec<(String, String)> = by_path
                    .iter()
                    .filter_map(|(p, id)| {
                        under(p, old).map(|rest| (format!("{new}{rest}"), id.clone()))
                    })
                    .collect();
                by_path.retain(|p, _| under(p, old).is_none() && under(p, new).is_none());
                by_id.retain(|_, p| under(p, old).is_none() && under(p, new).is_none());
                for (p, id) in moving {
                    by_path.insert(p.clone(), id.clone());
                    by_id.insert(id, p);
                }
            }
            #[cfg(feature = "sqlite")]
            Store::Sqlite { db, root } => {
                if let Err(e) = sqlite::moved(db, root, old, new) {
                    tracing::warn!("moving file ids from {old} to {new}: {e}");
                }
            }
        }
    }

    /// `path` (a file, or a directory and everything under it) was
    /// deleted.
    pub fn removed(&self, path: &str) {
        match &mut *self.lock() {
            Store::Memory { by_path, by_id } => {
                by_path.retain(|p, _| under(p, path).is_none());
                by_id.retain(|_, p| under(p, path).is_none());
            }
            #[cfg(feature = "sqlite")]
            Store::Sqlite { db, root } => {
                if let Err(e) = sqlite::removed(db, root, path) {
                    tracing::warn!("dropping file ids under {path}: {e}");
                }
            }
        }
    }
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
            );",
        )?;
        Ok(db)
    }

    pub fn index(db: &Connection, root: &str, path: &str) -> rusqlite::Result<(String, bool)> {
        let found: Option<String> = db
            .query_row(
                "SELECT id FROM file_ids WHERE root = ?1 AND path = ?2",
                params![root, path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = found {
            return Ok((id, false));
        }
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO file_ids (id, root, path) VALUES (?1, ?2, ?3)",
            params![id, root, path],
        )?;
        Ok((id, true))
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
            &format!("DELETE FROM file_ids WHERE {UNDER}"),
            params![root, path],
        )?;
        Ok(())
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
    }
}
