//! Registrations and claims on API paths.
//!
//! One side registers under the paths it is working on; the other side
//! claims paths, which holds back new registrations under them and waits
//! for the ones already under them. A claim covers either a prefix, itself
//! and everything under it as a directory, the rule file ids follow for
//! renames ([`ganymede_contents::path_under`]), or only the direct children
//! of a directory, where a create makes its new file.
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
    blocked: Vec<(String, Scope)>,
    /// How many registrations are parked by path, held back by a claim.
    waiting: HashMap<String, usize>,
}

/// What a claimed path covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// The path and everything under it.
    Subtree,
    /// The direct children of the directory: not it, not deeper.
    Children,
}

/// Whether the prefix `pre` covers `path`: the path itself, anything
/// under it as a directory, or everything for the root's empty prefix.
/// Both are normalized API paths.
pub(crate) fn covers(pre: &str, path: &str) -> bool {
    ganymede_contents::path_under(path, pre).is_some()
}

/// Whether a claim of `pre` with `scope` covers `path`. For `Children`,
/// the path's parent is `pre`, the root's `""` for a top-level path.
fn covered(scope: Scope, pre: &str, path: &str) -> bool {
    match scope {
        Scope::Subtree => covers(pre, path),
        Scope::Children => !path.is_empty() && crate::folder::dir_of(path) == pre,
    }
}

impl State {
    fn blocks_any(&self, paths: &[String]) -> bool {
        self.blocked
            .iter()
            .any(|(pre, scope)| paths.iter().any(|p| covered(*scope, pre, p)))
    }

    fn registered_under(&self, claims: &[(String, Scope)]) -> bool {
        Self::any_under(self.in_flight.keys(), claims)
    }

    fn waiting_under(&self, claims: &[(String, Scope)]) -> bool {
        Self::any_under(self.waiting.keys(), claims)
    }

    fn any_under<'a>(
        mut paths: impl Iterator<Item = &'a String>,
        claims: &[(String, Scope)],
    ) -> bool {
        paths.any(|p| claims.iter().any(|(pre, scope)| covered(*scope, pre, p)))
    }

    fn unpark(&mut self, paths: &[String]) {
        for path in paths {
            if let Some(n) = self.waiting.get_mut(path) {
                *n -= 1;
                if *n == 0 {
                    self.waiting.remove(path);
                }
            }
        }
    }
}

/// A registration parked behind a claim, until it registers or its wait
/// is dropped.
struct Parked {
    locks: Arc<PathLocks>,
    paths: Vec<String>,
}

impl Drop for Parked {
    fn drop(&mut self) {
        self.locks.state().unpark(&self.paths);
        self.locks.changed.notify_waiters();
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
    paths: Vec<(String, Scope)>,
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
    /// while a claim covers any of them, and is parked meanwhile: a claim
    /// made after that waits for the parked registration to go ahead
    /// first, so a stream of overlapping claims can't keep it waiting.
    pub(crate) async fn register(self: &Arc<Self>, paths: &[&str]) -> Registered {
        let paths: Vec<String> = paths.iter().map(|p| normalize_api_path(p)).collect();
        let mut parked: Option<Parked> = None;
        loop {
            // Listening before the state is read, so a change between the
            // read and the wait isn't missed.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut s = self.state();
                if !s.blocks_any(&paths) {
                    if let Some(p) = parked.as_mut() {
                        // Leaves the parked count and registers in one
                        // step, which is defensive: a claim waiting for
                        // the parked would see the registration either
                        // way. `p`'s drop, after this lock is released,
                        // only wakes the waiters.
                        s.unpark(&p.paths);
                        p.paths.clear();
                    }
                    for p in &paths {
                        *s.in_flight.entry(p.clone()).or_default() += 1;
                    }
                    return Registered {
                        locks: self.clone(),
                        paths,
                    };
                }
                if parked.is_none() {
                    for p in &paths {
                        *s.waiting.entry(p.clone()).or_default() += 1;
                    }
                    parked = Some(Parked {
                        locks: self.clone(),
                        paths: paths.clone(),
                    });
                }
            }
            notified.await;
        }
    }

    /// Claims `paths` at once, so nothing registers under any, then waits
    /// for the registrations under them, files or a directory's, until
    /// the guard is dropped. The claim is the guard's, so it goes if the
    /// wait is dropped. Claims don't wait for each other, but a claim
    /// first waits for the registrations parked behind an earlier claim
    /// under its paths to go ahead, and takes no hold until they have.
    pub(crate) async fn claim(self: &Arc<Self>, paths: &[&str]) -> Claimed {
        self.claim_scoped(paths, Scope::Subtree).await
    }

    /// [`PathLocks::claim`] with the given `scope`: `Scope::Children`
    /// covers only the direct children of each directory, so the
    /// registrations that wait are those of paths directly in it, and the
    /// directory itself and the paths deeper are neither held back nor
    /// waited for.
    pub(crate) async fn claim_scoped(self: &Arc<Self>, paths: &[&str], scope: Scope) -> Claimed {
        let paths: Vec<(String, Scope)> = paths
            .iter()
            .map(|p| (normalize_api_path(p), scope))
            .collect();
        // Not claimed yet while a registration under the paths is parked
        // behind another claim: it goes ahead first, whatever claims
        // follow it. Nothing is held until the claim is made, so a drop
        // here leaves nothing behind.
        let guard = loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut s = self.state();
                if !s.waiting_under(&paths) {
                    s.blocked.extend(paths.iter().cloned());
                    break Claimed {
                        locks: self.clone(),
                        paths,
                    };
                }
            }
            notified.await;
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

    /// A children claim covers the paths directly in its directory.
    #[test]
    fn a_children_claim_covers_direct_children_only() {
        let c = |pre, path| covered(Scope::Children, pre, path);
        assert!(c("", "a.ipynb"));
        assert!(!c("", "d/a.ipynb"));
        assert!(!c("", ""));
        assert!(c("d", "d/a"));
        assert!(!c("d", "d"));
        assert!(!c("d", "d/e/a"));
        assert!(!c("d", "dd/a"));
        assert!(!c("d", "a"));
    }

    /// A children claim of the root waits for and holds back top-level
    /// paths only.
    #[tokio::test]
    async fn a_children_claim_ignores_paths_deeper() {
        let locks = Arc::new(PathLocks::default());
        let deep = locks.register(&["d/a.ipynb"]).await;
        let top = locks.register(&["a.ipynb"]).await;
        let l = locks.clone();
        let claim = tokio::spawn(async move { l.claim_scoped(&[""], Scope::Children).await });
        tokio::time::sleep(SHORT).await;
        assert!(!claim.is_finished(), "waits for a.ipynb");
        drop(top);
        let claimed = tokio::time::timeout(LONG, claim).await.unwrap().unwrap();
        tokio::time::timeout(LONG, locks.register(&["d/b.ipynb"]))
            .await
            .expect("a path deeper is not held");
        let l = locks.clone();
        let late = tokio::spawn(async move { l.register(&["b.ipynb"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!late.is_finished(), "a top-level path waits");
        drop(claimed);
        tokio::time::timeout(LONG, late).await.unwrap().unwrap();
        drop(deep);
    }

    /// A registration parked behind a claim goes ahead before a claim
    /// made after it lands, so overlapping claims can't starve it.
    #[tokio::test]
    async fn overlapping_claims_do_not_starve_a_parked_registration() {
        let locks = Arc::new(PathLocks::default());
        let first = locks.claim(&["d"]).await;
        let l = locks.clone();
        let parked = tokio::spawn(async move { l.register(&["d/a.ipynb"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!parked.is_finished());
        let l = locks.clone();
        let second = tokio::spawn(async move { l.claim(&["d"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!second.is_finished(), "waits for the parked registration");
        drop(first);
        let registered = tokio::time::timeout(LONG, parked)
            .await
            .expect("the parked registration goes ahead")
            .unwrap();
        drop(registered);
        tokio::time::timeout(LONG, second).await.unwrap().unwrap();
    }

    /// A claim dropped while it waits for parked registrations leaves no
    /// claim behind, and a parked registration dropped leaves no count.
    #[tokio::test]
    async fn dropped_waits_leave_nothing_behind() {
        let locks = Arc::new(PathLocks::default());
        let first = locks.claim(&["d"]).await;
        let l = locks.clone();
        let parked = tokio::spawn(async move { l.register(&["d/a.ipynb"]).await });
        tokio::time::sleep(SHORT).await;
        let l = locks.clone();
        let second = tokio::spawn(async move { l.claim(&["d"]).await });
        tokio::time::sleep(SHORT).await;
        second.abort();
        let _ = second.await;
        parked.abort();
        let _ = parked.await;
        drop(first);
        assert!(locks.none_claimed());
        assert!(locks.state().waiting.is_empty());
        tokio::time::timeout(LONG, locks.claim(&["d"]))
            .await
            .unwrap();
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
