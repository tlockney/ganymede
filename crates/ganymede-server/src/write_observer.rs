//! Letting an app see the server's own writes (#916, #978).
//!
//! An app that keeps answers per notebook file (a link allowed "always", a
//! device permission) forgets them when the file is replaced outside
//! Ganymede, and keeps them when Ganymede writes it. It can stamp a file
//! itself ([`ganymede_contents::file_stamp`]), but most writes on its
//! server come from the server, with no window involved: a room's
//! autosave, a run saving its outputs, MCP. So the server reports each
//! such write where the app can hear of it, with the file's stamp just
//! before the write and just after it, and the app moves its answers from
//! one stamp to the other: to a function the app sets in a
//! [`WriteObserver`] when it embeds the server, and to any client of
//! `/api/events/subscribe` as a write event, so a client of a server it
//! does not embed hears the same.
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
use tokio::sync::broadcast;

/// The schema id of a write event on `/api/events/subscribe`
/// ([`WriteEvent::envelope`]): a client that knows it acts on the event,
/// and one that doesn't ignores it.
pub const WRITE_SCHEMA: &str = "https://ganymede-server.com/events/write/v1";

/// What the app is called with after a write: the file's canonical
/// absolute path (links resolved), its stamp just before the write and just
/// after (`None` when it could not be read, such as a file that did not
/// exist before). On Windows the canonical path has the `\\?\` prefix.
/// Called on a thread of the server, after the write and its
/// restamp, with the path's turn held: the next write or a rename of that
/// path waits for it, so it should return quickly. The observer's own lock
/// is not held, so it may call [`WriteObserver::set`].
pub type WriteObserverFn = dyn Fn(&Path, Option<&str>, Option<&str>) + Send + Sync;

/// One write the server made of a file, as `/api/events/subscribe` reports
/// it: the file's contents-API path as the write named it, relative to the
/// server's root and `/`-separated (never canonical or absolute, so the
/// event says nothing a client could not already list), and its stamps
/// just before and just after the write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteEvent {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// When the write was reported, once for every subscriber.
    pub at: jiff::Timestamp,
}

impl WriteEvent {
    /// The write in `jupyter_events`' envelope: its schema id and
    /// versions, when the write was reported, and its data. A client that
    /// doesn't know the schema ignores it, as JupyterLab's event manager
    /// does. The timestamp is RFC 3339 in UTC, as the server's own time
    /// crate, `jiff`, writes it.
    pub fn envelope(&self) -> serde_json::Value {
        serde_json::json!({
            "__schema__": WRITE_SCHEMA,
            "__schema_version__": 1,
            "__metadata_version__": 1,
            "__timestamp__": self.at.to_string(),
            "path": self.path,
            "before": self.before,
            "after": self.after,
        })
    }
}

/// How many writes the events channel holds for a subscriber that falls
/// behind before it is told it missed some.
const EVENTS: usize = 256;

/// The function called after each write of a local file, which an app can
/// replace or remove while the server runs, and the channel the events
/// socket reads the same writes from. Kept by the app and passed in
/// [`crate::serve::Startup::write_observer`], as the machines are; a
/// server built without one makes its own, so the socket works either way.
pub struct WriteObserver {
    function: RwLock<Option<Arc<WriteObserverFn>>>,
    events: broadcast::Sender<WriteEvent>,
}

impl Default for WriteObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteObserver {
    /// An observer with no function: no write is reported to an embedding
    /// app, though a client of the events socket still hears of them.
    pub fn new() -> Self {
        Self::with_capacity(EVENTS)
    }

    /// [`WriteObserver::new`], with room for `capacity` writes to wait for
    /// a subscriber that falls behind: a small one lets a test show what
    /// happens when one does.
    pub fn with_capacity(capacity: usize) -> Self {
        let (events, _) = broadcast::channel(capacity.max(1));
        WriteObserver {
            function: RwLock::new(None),
            events,
        }
    }

    /// Reports later writes to `f` instead of the one set before, or to no
    /// one when `None`. A write that began while none was set is not
    /// reported; one that began while one was set is reported to whatever
    /// is set when it ends.
    pub fn set(&self, f: Option<Arc<WriteObserverFn>>) {
        // The old function is dropped after the lock is let go: an app's
        // `release` may run then, and may call `set` itself.
        let old = std::mem::replace(
            &mut *self.function.write().unwrap_or_else(|e| e.into_inner()),
            f,
        );
        drop(old);
    }

    /// The writes the server makes from now on, for one client of
    /// `/api/events/subscribe`. Holding the receiver is what makes a write
    /// worth stamping before, so a server nobody listens to stamps
    /// nothing.
    pub fn subscribe(&self) -> broadcast::Receiver<WriteEvent> {
        self.events.subscribe()
    }

    /// Whether a write is worth stamping before: a function is set to hear
    /// of it, or the events socket has a subscriber.
    pub(crate) fn is_set(&self) -> bool {
        self.events.receiver_count() > 0 || self.has_function()
    }

    /// Whether a function is set now: a write that begins without one is
    /// not reported to one set before it ends (#916), even when an events
    /// socket's subscriber had its stamp taken.
    pub(crate) fn has_function(&self) -> bool {
        self.function
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Sends the event to the events socket, and calls the function with
    /// the write if one is set and `to_function` (one was set when the
    /// write began). The function is cloned out of the lock first, so it
    /// may set the observer. The caller's path turn is still held while
    /// both run. `api` is the file's contents-API path, which is what the
    /// event carries; the function keeps the canonical one.
    pub(crate) fn notify(
        &self,
        path: &Path,
        api: &str,
        before: Option<&str>,
        after: Option<&str>,
        to_function: bool,
    ) {
        // Sent first, so an app's function holds up no events socket. The
        // error is a write no socket is subscribed to.
        let _ = self.events.send(WriteEvent {
            path: api.to_string(),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
            at: jiff::Timestamp::now(),
        });
        if !to_function {
            return;
        }
        let f = self
            .function
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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

    /// A write goes to the app's function with its canonical path and to
    /// the events socket with its contents-API path, both once.
    #[tokio::test]
    async fn a_write_reaches_the_function_and_the_socket() {
        let observer = WriteObserver::new();
        let mut events = observer.subscribe();
        assert!(observer.is_set(), "a subscriber is listening");
        let calls: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>> = Default::default();
        let into = calls.clone();
        observer.set(Some(Arc::new(move |p, b, a| {
            assert_eq!((b, a), (Some("before"), Some("after")));
            into.lock().unwrap().push(p.to_path_buf());
        })));
        observer.notify(
            Path::new("/root/demo.ipynb"),
            "demo.ipynb",
            Some("before"),
            Some("after"),
            true,
        );
        assert_eq!(*calls.lock().unwrap(), [Path::new("/root/demo.ipynb")]);
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event")
            .expect("not lagged");
        assert_eq!(event.path, "demo.ipynb");
        assert_eq!(event.before.as_deref(), Some("before"));
        assert_eq!(event.after.as_deref(), Some("after"));
        // In jupyter_events' envelope, with the timestamp in UTC.
        let envelope = event.envelope();
        assert_eq!(envelope["__schema__"], WRITE_SCHEMA);
        assert_eq!(envelope["__schema_version__"], 1);
        assert_eq!(envelope["__metadata_version__"], 1);
        let at = envelope["__timestamp__"].as_str().unwrap();
        assert_eq!(
            at.parse::<jiff::Timestamp>().unwrap().to_string(),
            at,
            "RFC 3339 UTC: {at}"
        );
        assert_eq!(envelope["path"], "demo.ipynb");
        assert_eq!(envelope["before"], "before");
        assert_eq!(envelope["after"], "after");
    }

    /// Nothing is worth stamping until a subscriber or a function is
    /// there, and a dropped subscriber stops it again.
    #[tokio::test]
    async fn no_listener_no_write_is_worth_stamping() {
        let observer = WriteObserver::new();
        assert!(!observer.is_set());
        let events = observer.subscribe();
        assert!(observer.is_set());
        drop(events);
        assert!(!observer.is_set());
        observer.set(Some(Arc::new(
            |_: &Path, _: Option<&str>, _: Option<&str>| {},
        )));
        assert!(observer.is_set());
    }
}
