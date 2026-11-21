//! Letting an embedding app see the server's own writes (#916).
//!
//! An app that keeps answers per notebook file (a link allowed "always", a
//! device permission) forgets them when the file is replaced outside
//! Ganymede, and keeps them when Ganymede writes it. It can stamp a file
//! itself ([`ganymede_contents::file_stamp`]), but most writes on its
//! server come from the server, with no window involved: a room's
//! autosave, a run saving its outputs, MCP. So the server reports each
//! such write to a [`WriteObserver`] the app keeps, with the file's stamp
//! just before the write and just after it, and the app moves its answers
//! from one stamp to the other.
//!
//! Reported, once each and after it succeeded, when the contents are a
//! local directory: any save of a regular file, a new file's included, a
//! checkpoint restore, and a copy to a named destination path. Not reported: a write that fails; a
//! copy into a directory, a new untitled file, a rename and a delete; a
//! directory save. Renames are not reported, so an app that keys its
//! answers by path loses them on a rename it did not make itself, which
//! fails safe.

use std::path::Path;
use std::sync::{Arc, RwLock};

/// What the app is called with after a write: the file's canonical
/// absolute path (links resolved), its stamp just before the write and just
/// after (`None` when it could not be read, such as a file that did not
/// exist before). On Windows the canonical path has the `\\?\` prefix.
/// Called on a thread of the server, after the write and its
/// restamp, with the path's turn held: the next write or a rename of that
/// path waits for it, so it should return quickly. The observer's own lock
/// is not held, so it may call [`WriteObserver::set`].
pub type WriteObserverFn = dyn Fn(&Path, Option<&str>, Option<&str>) + Send + Sync;

/// The function called after each write of a local file, which an app can
/// replace or remove while the server runs. Kept by the app and passed in
/// [`crate::serve::Startup::write_observer`], as the machines are.
#[derive(Default)]
pub struct WriteObserver(RwLock<Option<Arc<WriteObserverFn>>>);

impl WriteObserver {
    /// An observer with no function: no write is reported.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports later writes to `f` instead of the one set before, or to no
    /// one when `None`. A write that began while none was set is not
    /// reported; one that began while one was set is reported to whatever
    /// is set when it ends.
    pub fn set(&self, f: Option<Arc<WriteObserverFn>>) {
        // The old function is dropped after the lock is let go: an app's
        // `release` may run then, and may call `set` itself.
        let old = std::mem::replace(&mut *self.0.write().unwrap_or_else(|e| e.into_inner()), f);
        drop(old);
    }

    /// Whether a function is set, so a write is worth stamping before.
    pub(crate) fn is_set(&self) -> bool {
        self.0.read().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Calls the function with the write, if one is set. It is cloned
    /// out of the lock first, so the function may set the observer. The
    /// caller's path turn is still held while it runs.
    pub(crate) fn notify(&self, path: &Path, before: Option<&str>, after: Option<&str>) {
        let f = self.0.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(f) = f {
            f(path, before, after);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Calls `set` on its observer when dropped, as an app's `release`
    /// may when its function is replaced.
    struct SetsOnDrop(Arc<WriteObserver>);

    impl Drop for SetsOnDrop {
        fn drop(&mut self) {
            self.0.set(None);
        }
    }

    /// Dropping the replaced function runs outside the lock (#918): a
    /// drop that calls `set` would otherwise deadlock on it.
    #[test]
    fn a_replaced_function_may_set_the_observer_when_dropped() {
        let observer = Arc::new(WriteObserver::new());
        let guard = SetsOnDrop(observer.clone());
        observer.set(Some(Arc::new(
            move |_: &Path, _: Option<&str>, _: Option<&str>| {
                let _ = &guard;
            },
        )));
        let (done, finished) = mpsc::channel();
        let replacing = observer.clone();
        std::thread::spawn(move || {
            replacing.set(Some(Arc::new(
                |_: &Path, _: Option<&str>, _: Option<&str>| {},
            )));
            let _ = done.send(());
        });
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("set deadlocked or panicked dropping the old function");
        // The old function's drop cleared what the call had just set.
        assert!(!observer.is_set());
    }
}
