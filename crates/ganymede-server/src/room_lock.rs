//! Which server on this machine owns a notebook's room (#272).
//!
//! Two servers with a room open on one file would both autosave it and
//! each see the other's saves as outside changes. So a room takes an
//! exclusive lock on a file in the Jupyter runtime dir named for the
//! notebook's canonical path (`flock` on Unix, `LockFileEx` on Windows,
//! through [`std::fs::File::try_lock`]) and holds it while it is open. A
//! room that can't take it is read-only: it shows the owner's saves and
//! refuses edits. The operating system drops the lock when its process
//! exits, however it exits, so a crashed owner leaves nothing stale.
//!
//! The lock files are empty and are never deleted: removing one while
//! another server is about to lock it would let two servers own the file.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What [`Locks::acquire`] found.
pub(crate) enum Acquire {
    /// This room owns the file while it holds this.
    Held(RoomLock),
    /// Another process owns it.
    Elsewhere,
    /// No locking here: no lock dir, a file not on disk, or an error
    /// (logged). The room is writable, as before #272.
    Unavailable,
}

/// The locks one server holds, by canonical path, so it can tell its own
/// lock from another process's: a second `flock` on the same file in this
/// process fails just as one from elsewhere would.
#[derive(Default)]
pub(crate) struct Locks {
    dir: Option<PathBuf>,
    held: Arc<Mutex<HashSet<PathBuf>>>,
}

/// A held lock; dropping it releases the file.
pub(crate) struct RoomLock {
    _file: File,
    path: PathBuf,
    held: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Drop for RoomLock {
    fn drop(&mut self) {
        self.held.lock().expect("held locks").remove(&self.path);
    }
}

impl Locks {
    /// Locks kept in `dir`, or none when `None` (an embedding app).
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Locks {
            dir,
            held: Arc::default(),
        }
    }

    /// Try to own `file` (an absolute path on disk).
    pub(crate) fn acquire(&self, file: &Path) -> Acquire {
        let Some(dir) = &self.dir else {
            return Acquire::Unavailable;
        };
        let Ok(canonical) = file.canonicalize() else {
            return Acquire::Unavailable;
        };
        if self.held.lock().expect("held locks").contains(&canonical) {
            // Another room of this server already owns it; rooms are one
            // per file id, so this is a moment's overlap at most.
            return Acquire::Unavailable;
        }
        let lock_file = match open(dir, &canonical) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("cannot open a room lock in {}: {e}", dir.display());
                return Acquire::Unavailable;
            }
        };
        match lock_file.try_lock() {
            Ok(()) => {
                self.held
                    .lock()
                    .expect("held locks")
                    .insert(canonical.clone());
                Acquire::Held(RoomLock {
                    _file: lock_file,
                    path: canonical,
                    held: self.held.clone(),
                })
            }
            Err(TryLockError::WouldBlock) => Acquire::Elsewhere,
            Err(TryLockError::Error(e)) => {
                tracing::warn!("cannot lock a room lock in {}: {e}", dir.display());
                Acquire::Unavailable
            }
        }
    }

    /// Whether another process owns `file`, for the contents API's
    /// `writable`. Takes and drops the lock to find out, without marking
    /// it held, so a room of this server opening at the same moment isn't
    /// turned away. While the probe holds it, another server's room
    /// opening on the file finds it owned and starts read-only; it takes
    /// the lock on its next retry, within a second.
    pub(crate) fn owned_elsewhere(&self, file: &Path) -> bool {
        let Some(dir) = &self.dir else {
            return false;
        };
        let Ok(canonical) = file.canonicalize() else {
            return false;
        };
        if self.held.lock().expect("held locks").contains(&canonical) {
            return false;
        }
        match open(dir, &canonical).map(|f| f.try_lock()) {
            Ok(Err(TryLockError::WouldBlock)) => true,
            // Ok(Ok(())): free; the lock drops with the file here.
            _ => false,
        }
    }
}

fn open(dir: &Path, canonical: &Path) -> std::io::Result<File> {
    crate::server_info::create_private_dir(dir)?;
    let name = hex::encode(Sha256::digest(canonical.as_os_str().as_encoded_bytes()));
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{name}.lock")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_owner_at_a_time_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("a.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        let locks_dir = Some(dir.path().join("locks"));
        // Two servers: separate lock sets, as in two processes.
        let one = Locks::new(locks_dir.clone());
        let two = Locks::new(locks_dir);

        let Acquire::Held(held) = one.acquire(&nb) else {
            panic!("the first server owns it")
        };
        assert!(matches!(two.acquire(&nb), Acquire::Elsewhere));
        assert!(two.owned_elsewhere(&nb));
        // Its own lock is not "elsewhere" to the owner.
        assert!(!one.owned_elsewhere(&nb));
        // The same file however it is spelled.
        let spelled = dir.path().join(".").join("a.ipynb");
        assert!(matches!(two.acquire(&spelled), Acquire::Elsewhere));

        drop(held);
        assert!(!two.owned_elsewhere(&nb));
        assert!(matches!(two.acquire(&nb), Acquire::Held(_)));
    }

    #[test]
    fn no_dir_or_no_file_means_no_locking() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("a.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        assert!(matches!(
            Locks::new(None).acquire(&nb),
            Acquire::Unavailable
        ));
        let locks = Locks::new(Some(dir.path().join("locks")));
        assert!(matches!(
            locks.acquire(&dir.path().join("missing.ipynb")),
            Acquire::Unavailable
        ));
    }
}
