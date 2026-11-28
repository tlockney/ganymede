//! The opt-in check for a newer release (#956): with `check_for_updates`
//! on, the server asks the `ganymede` binary for the latest release's
//! version, once at start and then once a day, and `/api/update` reports
//! what it found. Off by default, so nothing is asked of anyone unless a
//! person turns it on (`docs/security.md`).
//!
//! The check is a function `ganymede-cli` hands over ([`UpdateCheck`]),
//! which keeps every HTTP client out of this crate.

use crate::serve::UpdateCheck;
use std::cmp::Ordering;
use std::sync::Mutex;
use std::time::Duration;

/// How often [`Update::start`] checks, after the first check at start.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What a check found, kept from one start of a check to the next.
#[derive(Debug, Clone, Default)]
struct Status {
    /// The latest release's version, as the check last answered it.
    latest: Option<String>,
    /// When the last check that answered ran, RFC 3339 in UTC.
    checked_at: Option<String>,
    /// Whether the last check failed. A failure keeps the last
    /// `latest` and `checked_at`, so the notice the UI shows doesn't
    /// disappear because GitHub was unreachable.
    last_check_failed: bool,
}

/// `GET /api/update`, as `jupyter_server`'s own routes answer: one object
/// with everything the web UI needs for its notice, or to say why there
/// is none.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateModel {
    /// Whether checks are on: `check_for_updates` is set and the binary
    /// gave a way to check.
    pub enabled: bool,
    /// The version this server runs, such as `0.18.0`, or
    /// `0.18.0+main.<commit>` for a build of main.
    pub running: String,
    /// The latest release's version, once a check has answered.
    pub latest: Option<String>,
    /// Whether `latest` is newer than `running`.
    pub update_available: bool,
    /// When the last successful check ran, RFC 3339 in UTC.
    pub checked_at: Option<String>,
    /// Whether the last check failed.
    pub last_check_failed: bool,
    /// Whether this is a build of main, which is not checked: its
    /// release is only known once it is downloaded (`ganymede update
    /// --check`).
    pub main_build: bool,
}

/// The server's update check: the version it runs, the function that
/// finds the latest release, and what the last check found.
pub struct Update {
    running: String,
    check: Option<UpdateCheck>,
    /// Both `check_for_updates` and a check: without either nothing runs.
    enabled: bool,
    status: Mutex<Status>,
}

impl Default for Update {
    fn default() -> Self {
        Update::new(env!("CARGO_PKG_VERSION"), false, None)
    }
}

/// Whether `version` is a build of main's. `release.yml` writes those as
/// `0.18.0+main.<commit>`, which `ganymede --version` prints; such a
/// build's next release is the latest main build, and its version is
/// only known once it is downloaded, so it is never compared.
pub fn is_main_build(version: &str) -> bool {
    version.contains("+main")
}

/// `version` without its `+build` part.
fn base_version(version: &str) -> &str {
    version.split('+').next().unwrap_or(version)
}

/// When a check ran, as RFC 3339 in UTC and to the second:
/// `2026-11-28T01:00:00Z`.
fn checked_at_now() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

impl Update {
    /// The check `running` (this server's version) makes: `on` is
    /// `check_for_updates`, and `check` is what the binary gave, if it
    /// gave one. Without both, [`Update::start`] does nothing.
    pub fn new(running: impl Into<String>, on: bool, check: Option<UpdateCheck>) -> Self {
        let enabled = on && check.is_some();
        Update {
            running: running.into(),
            check,
            enabled,
            status: Mutex::new(Status::default()),
        }
    }

    /// The status, a failed check having poisoned nothing it wrote.
    fn lock(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether checks are on.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether this server is a build of main, which is not checked.
    pub fn main_build(&self) -> bool {
        is_main_build(&self.running)
    }

    /// What `/api/update` answers.
    pub fn model(&self) -> UpdateModel {
        let main_build = self.main_build();
        let status = self.lock();
        let enabled = self.enabled;
        let update_available = enabled
            && status.latest.as_deref().is_some_and(|latest| {
                crate::lab::compare_versions(base_version(latest), base_version(&self.running))
                    == Ordering::Greater
            });
        UpdateModel {
            enabled,
            running: self.running.clone(),
            latest: status.latest.clone(),
            update_available,
            checked_at: status.checked_at.clone(),
            last_check_failed: enabled && status.last_check_failed,
            main_build,
        }
    }

    /// Check once, through a blocking thread for the check's own HTTP.
    async fn check(&self) {
        let Some(check) = self.check.clone() else {
            return;
        };
        let answer = tokio::task::spawn_blocking(move || check()).await;
        let mut status = self.lock();
        match answer {
            Ok(Ok(latest)) => {
                status.latest = Some(latest);
                status.checked_at = Some(checked_at_now());
                status.last_check_failed = false;
            }
            Ok(Err(e)) => {
                tracing::debug!("update check failed: {e:#}");
                status.last_check_failed = true;
            }
            Err(e) => {
                tracing::debug!("update check failed: {e}");
                status.last_check_failed = true;
            }
        }
    }

    /// Start checking: once now, then once every twenty-four hours
    /// (`tokio::time::interval`'s first tick is immediate). `None` — no
    /// task, and the check never called — unless checks are on and this
    /// isn't a build of main. The server aborts the task when it stops.
    pub fn start(self: &std::sync::Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if !self.enabled || self.main_build() {
            return None;
        }
        let update = self.clone();
        Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(UPDATE_CHECK_INTERVAL);
            loop {
                tick.tick().await;
                update.check().await;
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{is_main_build, Update, UpdateCheck};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A check that answers `"0.19.0"` and counts its calls.
    fn newer(calls: &Arc<AtomicUsize>) -> UpdateCheck {
        let calls = calls.clone();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("0.19.0".to_string())
        })
    }

    /// Waits for the task [`Update::start`] spawned to record an answer.
    async fn settle(update: &Update, answered: impl Fn(&super::UpdateModel) -> bool) {
        for _ in 0..200 {
            if answered(&update.model()) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the check never answered: {:?}", update.model());
    }

    #[test]
    fn a_main_build_has_a_build_suffix() {
        assert!(is_main_build("0.18.0+main.abc123"));
        assert!(!is_main_build("0.18.0"));
        assert!(!is_main_build("0.19.0-rc.1"));
    }

    /// A build of main is never checked: its release isn't a version on
    /// GitHub until it is downloaded, so there is nothing to compare.
    #[tokio::test]
    async fn a_main_build_is_not_checked() {
        let calls = Arc::new(AtomicUsize::new(0));
        let update = Arc::new(Update::new("0.18.0+main.abc123", true, Some(newer(&calls))));
        assert!(update.start().is_none());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let model = update.model();
        assert!(model.enabled, "checks are on, for other builds");
        assert!(model.main_build);
        assert_eq!(model.latest, None);
        assert_eq!(model.checked_at, None);
        assert!(!model.update_available);
    }

    #[tokio::test]
    async fn off_spawns_nothing_and_says_so() {
        let calls = Arc::new(AtomicUsize::new(0));
        let update = Arc::new(Update::new("0.18.0", false, Some(newer(&calls))));
        assert!(update.start().is_none());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let model = update.model();
        assert!(!model.enabled);
        assert_eq!(model.running, "0.18.0");
        assert_eq!(model.latest, None);
        assert!(!model.update_available);
        assert!(!model.last_check_failed);
    }

    /// An app's in-process server gives no check, so there is nothing to
    /// enable even with the setting on.
    #[test]
    fn no_check_is_not_enabled() {
        let update = Update::new("0.18.0", true, None);
        assert!(!update.enabled());
        assert!(!update.model().enabled);
    }

    #[tokio::test]
    async fn a_newer_release_is_reported() {
        let calls = Arc::new(AtomicUsize::new(0));
        let update = Arc::new(Update::new("0.18.0", true, Some(newer(&calls))));
        let task = update.start().expect("checks are on");
        settle(&update, |m| m.latest.is_some()).await;
        task.abort();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one check, at start");
        let model = update.model();
        assert!(model.enabled);
        assert!(!model.main_build);
        assert_eq!(model.latest.as_deref(), Some("0.19.0"));
        assert!(model.update_available);
        assert!(!model.last_check_failed);
        let at = model.checked_at.expect("a check that answered has a time");
        assert_eq!(at.len(), 20, "{at}");
        assert!(at.ends_with('Z'), "{at}");
        at.parse::<jiff::Timestamp>()
            .unwrap_or_else(|e| panic!("{at} is not RFC 3339: {e}"));
    }

    #[tokio::test]
    async fn the_same_version_is_not_newer() {
        let calls = Arc::new(AtomicUsize::new(0));
        let check: UpdateCheck = Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("0.18.0".to_string())
        });
        let update = Arc::new(Update::new("0.18.0", true, Some(check)));
        let task = update.start().expect("checks are on");
        settle(&update, |m| m.latest.is_some()).await;
        task.abort();
        let model = update.model();
        assert_eq!(model.latest.as_deref(), Some("0.18.0"));
        assert!(!model.update_available);
    }

    /// A failed check is logged, flagged, and leaves the last good answer
    /// where it was, so a notice doesn't come and go with the network.
    #[tokio::test]
    async fn a_failed_check_is_flagged_and_keeps_the_last_answer() {
        let calls = Arc::new(AtomicUsize::new(0));
        let answer = calls.clone();
        let check: UpdateCheck = Arc::new(move || {
            if answer.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok("0.19.0".to_string())
            } else {
                Err(anyhow::anyhow!("offline"))
            }
        });
        let update = Update::new("0.18.0", true, Some(check));
        update.check().await;
        let good = update.model();
        assert_eq!(good.latest.as_deref(), Some("0.19.0"));
        assert!(good.update_available);
        assert!(!good.last_check_failed);

        update.check().await;
        let bad = update.model();
        assert!(bad.last_check_failed);
        assert_eq!(bad.latest, good.latest, "the last good answer stays");
        assert_eq!(bad.checked_at, good.checked_at);
        assert!(bad.update_available, "the notice the UI shows stays");
    }
}
