//! File watching for document rooms: tells a room that its file may have
//! changed, so it can reload without polling.
//!
//! Rules from nteract's daemon (#141):
//!
//! - Watch the parent directory, not the file. A watch on the file follows
//!   the old inode and goes quiet after the first save by temp file and
//!   rename, which is how `ganymede-contents` and most editors write.
//! - Ignore reads (`IN_ACCESS` on Linux), which would cause reload loops.
//! - Debouncing and telling our own writes from others' is the room's job:
//!   it waits for events to settle, then compares the file's hash with
//!   the last one it read or wrote.
//!
//! Only Linux (inotify), macOS (FSEvents) and Windows are watched; on other
//! platforms, or when the OS refuses a watch, [`Watcher::watch`] returns
//! `None` and the room polls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

type Subscribers = HashMap<PathBuf, Vec<(u64, mpsc::UnboundedSender<()>)>>;

/// One OS watcher for all rooms of a server.
pub struct Watcher {
    files: Arc<Mutex<Subscribers>>,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    os: Mutex<Option<Os>>,
    next: std::sync::atomic::AtomicU64,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
struct Os {
    watcher: notify::RecommendedWatcher,
    /// Watched directories and how many files each serves.
    dirs: HashMap<PathBuf, usize>,
}

/// A file being watched; dropping it stops the watch.
pub struct Watch {
    watcher: Arc<Watcher>,
    file: PathBuf,
    id: u64,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.watcher.unwatch(&self.file, self.id);
    }
}

impl Watcher {
    pub fn new() -> Arc<Self> {
        let files: Arc<Mutex<Subscribers>> = Arc::default();
        Arc::new(Watcher {
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            os: Mutex::new(Os::new(files.clone())),
            files,
            next: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Watch `file` (an absolute path). The receiver gets a `()` for each
    /// event that may have changed it; several can arrive for one write.
    /// `None` when the platform or the OS gives no events.
    pub fn watch(self: &Arc<Self>, file: &Path) -> Option<(Watch, mpsc::UnboundedReceiver<()>)> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let dir = file.parent()?.to_path_buf();
            let mut os = self.os.lock().unwrap_or_else(|e| e.into_inner());
            let os = os.as_mut()?;
            if !os.dirs.contains_key(&dir) {
                use notify::Watcher as _;
                if let Err(e) = os.watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
                    tracing::info!("not watching {}: {e}; polling instead", dir.display());
                    return None;
                }
            }
            *os.dirs.entry(dir).or_default() += 1;
            let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (tx, rx) = mpsc::unbounded_channel();
            self.files
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(file.to_path_buf())
                .or_default()
                .push((id, tx));
            Some((
                Watch {
                    watcher: self.clone(),
                    file: file.to_path_buf(),
                    id,
                },
                rx,
            ))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = file;
            None
        }
    }

    fn unwatch(&self, file: &Path, id: u64) {
        {
            let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(subs) = files.get_mut(file) {
                subs.retain(|(i, _)| *i != id);
                if subs.is_empty() {
                    files.remove(file);
                }
            }
        }
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let Some(dir) = file.parent() else { return };
            let mut os = self.os.lock().unwrap_or_else(|e| e.into_inner());
            let Some(os) = os.as_mut() else { return };
            if let Some(n) = os.dirs.get_mut(dir) {
                *n -= 1;
                if *n == 0 {
                    os.dirs.remove(dir);
                    use notify::Watcher as _;
                    let _ = os.watcher.unwatch(dir);
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl Os {
    fn new(files: Arc<Mutex<Subscribers>>) -> Option<Self> {
        let handler = move |event: notify::Result<notify::Event>| {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("file watcher: {e}");
                    return;
                }
            };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            let files = files.lock().unwrap_or_else(|e| e.into_inner());
            let notify = |subs: &Vec<(u64, mpsc::UnboundedSender<()>)>| {
                for (_, tx) in subs {
                    let _ = tx.send(());
                }
            };
            if event.paths.is_empty() || event.need_rescan() {
                files.values().for_each(notify);
                return;
            }
            for path in &event.paths {
                if let Some(subs) = files.get(path) {
                    notify(subs);
                }
            }
        };
        match notify::recommended_watcher(handler) {
            Ok(watcher) => Some(Os {
                watcher,
                dirs: HashMap::new(),
            }),
            Err(e) => {
                tracing::info!("no file watcher ({e}); document rooms poll instead");
                None
            }
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn next(rx: &mut mpsc::UnboundedReceiver<()>) -> bool {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_replaced_file_is_still_watched_and_siblings_are_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let file = dir.join("a.ipynb");
        std::fs::write(&file, "1").unwrap();
        let watcher = Watcher::new();
        let (watch, mut rx) = watcher.watch(&file).expect("this platform has a watcher");

        // Saved by temp file and rename, twice: the second save is seen
        // too, which a watch on the file's inode would miss.
        for content in ["2", "3"] {
            let tmp = dir.join(".a.ipynb.tmp");
            std::fs::write(&tmp, content).unwrap();
            std::fs::rename(&tmp, &file).unwrap();
            assert!(next(&mut rx).await, "no event for the save of {content}");
            tokio::time::sleep(Duration::from_secs(1)).await;
            while rx.try_recv().is_ok() {}
        }

        // A sibling's change is not this file's.
        std::fs::write(dir.join("b.ipynb"), "x").unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(rx.try_recv().is_err());

        drop(watch);
        assert!(watcher.files.lock().unwrap().is_empty());
    }
}
