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
    /// Another room of this server holds it. A retiring room releases its
    /// lock before a new room can open, so this means two file ids for one
    /// file (a symlinked directory, a hard link, a case-insensitive name),
    /// which this doesn't solve: the room is writable and tries again
    /// (#400).
    HeldHere,
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
    /// `None` only while dropping.
    file: Option<File>,
    path: PathBuf,
    held: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Drop for RoomLock {
    fn drop(&mut self) {
        // Release the file under the guard, before it leaves the held set:
        // a probe in between would find it locked and not held here, and
        // report another process.
        let mut held = self.held.lock().expect("held locks");
        drop(self.file.take());
        held.remove(&self.path);
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
        // Held through the `try_lock`, so it never overlaps a probe of
        // this server's (`owned_elsewhere`): flock locks belong to the open
        // file, so the probe's lock would turn this room away as if
        // another process owned the file.
        let mut held = self.held.lock().expect("held locks");
        if held.contains(&canonical) {
            // Another room of this server owns it, under another file id.
            return Acquire::HeldHere;
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
                held.insert(canonical.clone());
                Acquire::Held(RoomLock {
                    file: Some(lock_file),
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
    /// `writable`. Takes and drops the lock to find out, holding this
    /// server's held set throughout, so a room of this server opening at
    /// the same moment waits rather than being turned away. While the
    /// probe holds it, another server's room opening on the file finds it
    /// owned and starts read-only; it takes the lock on its next retry,
    /// within a second.
    pub(crate) fn owned_elsewhere(&self, file: &Path) -> bool {
        let Some(dir) = &self.dir else {
            return false;
        };
        let Ok(canonical) = file.canonicalize() else {
            return false;
        };
        let held = self.held.lock().expect("held locks");
        if held.contains(&canonical) {
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
    fn a_probe_never_turns_this_servers_own_room_away() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("a.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        let locks = Arc::new(Locks::new(Some(dir.path().join("locks"))));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let prober = {
            let (locks, stop, nb) = (locks.clone(), stop.clone(), nb.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    assert!(!locks.owned_elsewhere(&nb));
                }
            })
        };
        for _ in 0..2000 {
            match locks.acquire(&nb) {
                Acquire::Held(lock) => drop(lock),
                Acquire::Elsewhere => panic!("this server's probe turned its room away"),
                _ => panic!("no locking"),
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        prober.join().unwrap();
    }

    #[test]
    fn a_second_room_in_one_server_is_told_to_retry() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("a.ipynb");
        std::fs::write(&nb, "{}").unwrap();
        let locks = Locks::new(Some(dir.path().join("locks")));
        let Acquire::Held(held) = locks.acquire(&nb) else {
            panic!("the first room owns it")
        };
        assert!(matches!(locks.acquire(&nb), Acquire::HeldHere));
        drop(held);
        assert!(matches!(locks.acquire(&nb), Acquire::Held(_)));
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
