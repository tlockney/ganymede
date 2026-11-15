//! Registrations and claims on API paths.
//!
//! One side registers under the paths it is working on; the other side
//! claims paths, which holds back new registrations under them and waits
//! for the ones already under them. A prefix covers itself and everything
//! under it as a directory, the rule file ids follow for renames
//! ([`ganymede_contents::path_under`]).
//!
//! Two instances are used: the server's writes against renames
//! ([`crate::stamping::Stamps`]), and grant lookups against creates
//! ([`crate::collab::Collab`]).

use ganymede_contents::normalize_api_path;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The paths registered and the paths claimed.
#[derive(Default)]
pub(crate) struct PathLocks {
    state: Mutex<State>,
    /// Woken whenever `state` loses a registration or a claim.
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct State {
    /// How many registrations there are by path.
    in_flight: HashMap<String, usize>,
    /// One entry per claim path, for a claim waiting or held.
    blocked: Vec<String>,
}

/// Whether the prefix `pre` covers `path`: the path itself, anything
/// under it as a directory, or everything for the root's empty prefix.
/// Both are normalized API paths.
pub(crate) fn covers(pre: &str, path: &str) -> bool {
    ganymede_contents::path_under(path, pre).is_some()
}

impl State {
    fn blocks_any(&self, paths: &[String]) -> bool {
        self.blocked
            .iter()
            .any(|pre| paths.iter().any(|p| covers(pre, p)))
    }

    fn registered_under(&self, prefixes: &[String]) -> bool {
        self.in_flight
            .keys()
            .any(|p| prefixes.iter().any(|pre| covers(pre, p)))
    }
}

/// Paths registered, until dropped.
pub(crate) struct Registered {
    locks: Arc<PathLocks>,
    paths: Vec<String>,
}

impl Drop for Registered {
    fn drop(&mut self) {
        let mut s = self.locks.state();
        for path in &self.paths {
            if let Some(n) = s.in_flight.get_mut(path) {
                *n -= 1;
                if *n == 0 {
                    s.in_flight.remove(path);
                }
            }
        }
        drop(s);
        self.locks.changed.notify_waiters();
    }
}

/// Paths claimed, until dropped, including while the claim still waits
/// for the registrations under them.
pub(crate) struct Claimed {
    locks: Arc<PathLocks>,
    paths: Vec<String>,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let mut s = self.locks.state();
        for path in &self.paths {
            if let Some(i) = s.blocked.iter().position(|b| b == path) {
                s.blocked.remove(i);
            }
        }
        drop(s);
        self.locks.changed.notify_waiters();
    }
}

impl PathLocks {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers under `paths`, which a claim of one of them, or of a
    /// directory above, waits for until the guard is dropped. Waits first
    /// while a claim covers any of them.
    pub(crate) async fn register(self: &Arc<Self>, paths: &[&str]) -> Registered {
        let paths: Vec<String> = paths.iter().map(|p| normalize_api_path(p)).collect();
        loop {
            // Listening before the state is read, so a change between the
            // read and the wait isn't missed.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut s = self.state();
                if !s.blocks_any(&paths) {
                    for p in &paths {
                        *s.in_flight.entry(p.clone()).or_default() += 1;
                    }
                    return Registered {
                        locks: self.clone(),
                        paths,
                    };
                }
            }
            notified.await;
        }
    }

    /// Claims `paths` at once, so nothing registers under any, then waits
    /// for the registrations under them, files or a directory's, until
    /// the guard is dropped. The claim is the guard's, so it goes if the
    /// wait is dropped. Claims don't wait for each other.
    pub(crate) async fn claim(self: &Arc<Self>, paths: &[&str]) -> Claimed {
        let paths: Vec<String> = paths.iter().map(|p| normalize_api_path(p)).collect();
        self.state().blocked.extend(paths.iter().cloned());
        let guard = Claimed {
            locks: self.clone(),
            paths,
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.state().registered_under(&guard.paths) {
                return guard;
            }
            notified.await;
        }
    }

    /// Whether anything is registered, for tests to wait on.
    #[cfg(test)]
    pub(crate) fn any_registered(&self) -> bool {
        !self.state().in_flight.is_empty()
    }

    /// Whether nothing is claimed, for tests.
    #[cfg(test)]
    pub(crate) fn none_claimed(&self) -> bool {
        self.state().blocked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: std::time::Duration = std::time::Duration::from_millis(50);
    const LONG: std::time::Duration = std::time::Duration::from_secs(10);

    /// A prefix covers itself and what is under it, not a sibling whose
    /// name starts the same, and the root's empty prefix covers all.
    #[test]
    fn a_prefix_covers_its_directory_only() {
        assert!(covers("d", "d"));
        assert!(covers("d", "d/x.ipynb"));
        assert!(covers("d", "d/e/x.ipynb"));
        assert!(!covers("d", "dd/x.ipynb"));
        assert!(!covers("d", "dd"));
        assert!(!covers("d/e", "d"));
        assert!(covers("", "d/x.ipynb"));
        assert!(covers("", ""));
    }

    /// A claim waits for a registration under it, and a registration
    /// waits for a claim over it, whichever instance is used.
    #[tokio::test]
    async fn a_claim_and_a_registration_wait_for_each_other() {
        let locks = Arc::new(PathLocks::default());
        let held = locks.register(&["d/a.ipynb"]).await;
        let l = locks.clone();
        let claim = tokio::spawn(async move { l.claim(&["d"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!claim.is_finished(), "the claim waits for the registration");
        drop(held);
        let claimed = tokio::time::timeout(LONG, claim).await.unwrap().unwrap();
        let l = locks.clone();
        let late = tokio::spawn(async move { l.register(&["d/b.ipynb"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!late.is_finished(), "the registration waits for the claim");
        tokio::time::timeout(LONG, locks.register(&["dd/x.ipynb"]))
            .await
            .expect("a sibling is not held");
        drop(claimed);
        tokio::time::timeout(LONG, late).await.unwrap().unwrap();
    }

    /// A claim dropped while it waits gives its paths back.
    #[tokio::test]
    async fn a_claim_dropped_while_waiting_gives_its_paths_back() {
        let locks = Arc::new(PathLocks::default());
        let held = locks.register(&["d/a.ipynb"]).await;
        let l = locks.clone();
        let claim = tokio::spawn(async move { l.claim(&["d"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!locks.none_claimed());
        claim.abort();
        let _ = claim.await;
        assert!(locks.none_claimed());
        drop(held);
    }
}
