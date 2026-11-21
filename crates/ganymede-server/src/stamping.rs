//! Keeping host grants through the server's own saves (#767).
//!
//! A notebook's host API grant is stored with a stamp of its file
//! ([`ganymede_contents::file_stamp`]), and dropped when the file no
//! longer matches it: a file replaced outside the server keeps its path's
//! file id, and its new content must not inherit the grant. A write the
//! server makes changes the stamp too, so [`Stamping`] wraps the server's
//! [`Contents`] and records the new stamp after each one.
//!
//! A read of a grant can land between a write and its restamp, and must
//! not take the write for an outside change. [`Stamps`] counts the saves
//! in flight by path, and a check that overlaps one, or that sees a save
//! of that path begin or end while it looks at the file, keeps the grant.

use crate::collab::{detached, Collab};
use crate::path_locks::{Claimed, PathLocks, Registered, Scope};
use crate::write_observer::WriteObserver;
use async_trait::async_trait;
use ganymede_contents::{
    normalize_api_path, Checkpoint, ContentType, Contents, ContentsModel, GetOptions, Result,
    SaveRequest,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Where the contents are on disk, and which of their files are being
/// written by the server now.
pub(crate) struct Stamps {
    /// `None` when the contents are not a local directory: no file has a
    /// stamp, so a look finds every file missing and nothing is stamped.
    /// Saves still take turns and are marked in flight.
    root: Option<PathBuf>,
    saving: Mutex<Saving>,
    turns: Turns,
    /// The paths the server's writes are registered under, and the
    /// paths of renames in progress. A rename waits only for the writes
    /// under its two paths, and holds back only new writes under them.
    writes: Arc<PathLocks>,
    /// Told of each write of a file under `root` (#916), when one is set.
    observer: Option<Arc<WriteObserver>>,
}

/// A write of a local file being watched: its path and its stamp before
/// the write ([`Stamps::observe`]).
pub(crate) struct Observed {
    file: PathBuf,
    before: Option<String>,
    observer: Arc<WriteObserver>,
}

impl Observed {
    /// The write succeeded: reports it with the file's stamp as it is
    /// now. Called once the file is restamped, inside the turn.
    pub(crate) fn done(self) {
        let after = ganymede_contents::file_stamp(&self.file);
        self.observer
            .notify(&self.file, self.before.as_deref(), after.as_deref());
    }
}

/// A write registered under some paths, until dropped.
pub(crate) type WriteGuard = Registered;

/// A rename's claim on its two paths, until dropped, including while it
/// still waits for the writes under them.
pub(crate) type RenameGuard = Claimed;

/// One server write of a path at a time: the lock for each path being
/// written or waited for.
type Turns = Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

/// A turn to write one path, until dropped.
pub(crate) struct Turn {
    stamps: Arc<Stamps>,
    path: String,
    _held: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut turns = self.stamps.turns.lock().unwrap_or_else(|e| e.into_inner());
        // Ours, and the map's: no one else is waiting.
        if turns
            .get(&self.path)
            .is_some_and(|m| Arc::strong_count(m) <= 2)
        {
            turns.remove(&self.path);
        }
    }
}

/// Saves of paths to look at: how many are in flight, and when each path
/// last had one begin or end.
#[derive(Default)]
struct Saving {
    paths: HashMap<String, PathSaves>,
    /// How many of `paths` have no save in flight. Only these are ever
    /// dropped, so saves in flight don't count towards [`REMEMBERED`].
    quiet: usize,
    /// Counts every begin and end, of any path.
    seq: u64,
    /// Quiet paths dropped from `paths` to keep it small are forgotten
    /// as of here, the latest `last` of any of them: a check that began
    /// before this can't say its path was quiet. It stays put while
    /// nothing is dropped.
    floor: u64,
}

#[derive(Default)]
struct PathSaves {
    in_flight: usize,
    /// `seq` at this path's last begin or end.
    last: u64,
}

/// The most paths remembered once their saves are over: a cleanup drops
/// them all, but the one that just ended, when more are.
const REMEMBERED: usize = 256;

/// How many times a look is made again after a cleanup of those.
const LOOKS: usize = 3;

/// A save of one path in flight, until dropped, however the save ends.
pub(crate) struct SaveGuard {
    stamps: Arc<Stamps>,
    path: String,
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        let mut s = self.stamps.lock();
        s.seq += 1;
        let seq = s.seq;
        if let Some(p) = s.paths.get_mut(&self.path) {
            p.in_flight -= 1;
            p.last = seq;
            if p.in_flight == 0 {
                s.quiet += 1;
            }
        }
        if s.quiet > REMEMBERED {
            let mut floor = s.floor;
            // The path that just ended stays, so the floor is the end of
            // an older one and a look begun before this end, but after
            // every older end, isn't voided.
            let ended = &self.path;
            s.paths.retain(|path, p| {
                let keep = p.in_flight > 0 || path == ended;
                if !keep {
                    floor = floor.max(p.last);
                }
                keep
            });
            s.quiet = 1;
            s.floor = floor;
        }
    }
}

/// What a look at a file's stamp for a grant check found.
pub(crate) enum Look {
    Stamp(String),
    /// The file can't be stat'ed.
    Missing,
    /// A save of this path was in flight, or began or ended meanwhile.
    Unstable,
}

impl Stamps {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Stamps {
            root,
            saving: Mutex::default(),
            turns: Mutex::default(),
            writes: Arc::default(),
            observer: None,
        }
    }

    /// Reports the server's writes to `observer` ([`Stamps::observe`]).
    pub(crate) fn with_observer(mut self, observer: Arc<WriteObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Starts watching the write of the file at `path`, inside its turn
    /// and before it is written: takes its stamp as it is. `None` when no
    /// function is set, or the contents are not a local directory, and
    /// then the write is not reported and no stamp is read.
    pub(crate) fn observe(&self, path: &str) -> Option<Observed> {
        let observer = self.observer.as_ref().filter(|o| o.is_set())?;
        let root = self.root.as_deref()?;
        let file = ganymede_contents::api_file_path(root, path)?;
        Some(Observed {
            before: ganymede_contents::file_stamp(&file),
            file,
            observer: observer.clone(),
        })
    }

    /// A write under `paths` (a file's, or the directory a new file is
    /// made in), registered until dropped, which a rename of one of them
    /// or of a directory above them waits for. Waits first while a
    /// rename holds any of them.
    pub(crate) async fn begin_write(self: &Arc<Self>, paths: &[&str]) -> WriteGuard {
        self.writes.register(paths).await
    }

    /// Claims `old` and `new` at once, so no write begins under either,
    /// then waits for the writes in flight under them, files or a
    /// directory's, until the guard is dropped. Taken after
    /// [`Collab::begin_move`]'s lock. The claim is the guard's, so it
    /// goes if the wait is dropped.
    pub(crate) async fn begin_rename(self: &Arc<Self>, old: &str, new: &str) -> RenameGuard {
        self.writes.claim(&[old, new]).await
    }

    /// Whether any write is registered, for tests to wait on.
    #[cfg(test)]
    fn any_write(&self) -> bool {
        self.writes.any_registered()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Saving> {
        self.saving.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for the turn to write the file at `path`.
    pub(crate) async fn turn(self: &Arc<Self>, path: &str) -> Turn {
        let path = normalize_api_path(path);
        let lock = self
            .turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(path.clone())
            .or_default()
            .clone();
        Turn {
            stamps: self.clone(),
            path,
            _held: Some(lock.lock_owned().await),
        }
    }

    /// The server is about to write the file at `path`.
    pub(crate) fn begin(self: &Arc<Self>, path: &str) -> SaveGuard {
        let path = normalize_api_path(path);
        let mut s = self.lock();
        s.seq += 1;
        let seq = s.seq;
        let was_quiet = match s.paths.get_mut(&path) {
            Some(p) => {
                let quiet = p.in_flight == 0;
                p.in_flight += 1;
                p.last = seq;
                quiet
            }
            None => {
                s.paths.insert(
                    path.clone(),
                    PathSaves {
                        in_flight: 1,
                        last: seq,
                    },
                );
                false
            }
        };
        if was_quiet {
            s.quiet -= 1;
        }
        SaveGuard {
            stamps: self.clone(),
            path,
        }
    }

    /// The stamp of the file at `path` as it is, to record after the
    /// server wrote it.
    pub(crate) fn own(&self, path: &str) -> Option<String> {
        ganymede_contents::api_file_stamp(self.root.as_deref()?, path)
    }

    /// Where a look at `path` starts, or `None` if a save of it is in
    /// flight.
    fn mark(&self, path: &str) -> Option<u64> {
        let s = self.lock();
        match s.paths.get(path) {
            Some(p) if p.in_flight > 0 => None,
            _ => Some(s.seq),
        }
    }

    /// Whether no save of `path` began or ended since `mark`. Saves of
    /// other paths don't count.
    fn stable_since(&self, path: &str, mark: u64) -> bool {
        let s = self.lock();
        mark >= s.floor
            && s.paths
                .get(path)
                .is_none_or(|p| p.in_flight == 0 && p.last <= mark)
    }

    /// The stamp of the file at `path` to check a grant against.
    pub(crate) fn look(&self, path: &str) -> Look {
        self.look_with(path, || {})
    }

    /// [`Stamps::look`], with `between` run after each look's mark and
    /// before its stamp is checked, for a test to make a save or a
    /// cleanup land there.
    fn look_with(&self, path: &str, mut between: impl FnMut()) -> Look {
        let path = normalize_api_path(path);
        // A look is retried when the cleanup of the remembered paths ran
        // meanwhile: it forgot this path too, so the look can't say it
        // was quiet, but a look begun after the cleanup can.
        for _ in 0..LOOKS {
            let Some(mark) = self.mark(&path) else {
                return Look::Unstable;
            };
            let Some(stamp) = self.own(&path) else {
                return Look::Missing;
            };
            between();
            if self.stable_since(&path, mark) {
                return Look::Stamp(stamp);
            }
            if self.lock().floor <= mark {
                return Look::Unstable;
            }
        }
        Look::Unstable
    }

    /// [`Stamps::look`] as a stamp to compare: `None` (keep the grant)
    /// when it can't be known.
    pub(crate) fn current(&self, path: &str) -> Option<String> {
        match self.look(path) {
            Look::Stamp(s) => Some(s),
            _ => None,
        }
    }
}

impl Collab {
    /// The server's contents, with every save that rewrites a file
    /// recording the new stamp of its grant. Install it as
    /// `AppState::contents`, so rooms, REST, MCP and run copies all keep
    /// their grants.
    pub fn stamping(self: &Arc<Self>, contents: Arc<dyn Contents>) -> Arc<dyn Contents> {
        Arc::new(Stamping {
            collab: self.clone(),
            inner: contents,
        })
    }
}

struct Stamping {
    collab: Arc<Collab>,
    inner: Arc<dyn Contents>,
}

impl Stamping {
    /// Rewrite the file at `path` with `write`, and keep its grant: one
    /// that its file already outgrew is dropped first, so this write
    /// doesn't bless what came from outside. Runs to the end if the
    /// caller goes away, so a write is never left unstamped, or
    /// unreported to the write observer (#916) once it succeeded.
    async fn rewrite<T: Send + 'static>(
        &self,
        path: &str,
        write: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let collab = self.collab.clone();
        let path = path.to_string();
        detached(async move {
            let _writing = collab.stamps.begin_write(&[&path]).await;
            // One save of a path at a time, so the stamp the check saw is
            // the one this save's restamp compares with.
            let _turn = collab.stamps.turn(&path).await;
            let seen = collab.check_for_write(&path).await;
            // The same file's grants under other roots in the shared
            // database get the same check and restamp.
            let others = collab.check_others(&path).await;
            let watched = collab.stamps.observe(&path);
            let _saving = collab.stamps.begin(&path);
            // A write that fails isn't restamped: whether it wrote the file
            // can't be known, and an If-Match failure over an outside
            // change must not bless it. The grant is then dropped on its
            // next read if the file did change, which fails safe.
            let out = write.await?;
            if let Err(e) = collab.restamp(&path, &seen).await {
                tracing::warn!("{e}");
            }
            collab.restamp_others(others).await;
            if let Some(watched) = watched {
                watched.done();
            }
            Ok(out)
        })
        .await
    }
}

#[async_trait]
impl Contents for Stamping {
    async fn get(&self, path: &str, options: GetOptions) -> Result<ContentsModel> {
        self.inner.get(path, options).await
    }

    async fn open_file(&self, path: &str) -> Result<Option<ganymede_contents::OpenFile>> {
        self.inner.open_file(path).await
    }

    async fn save(&self, path: &str, request: SaveRequest) -> Result<ContentsModel> {
        let inner = self.inner.clone();
        let p = path.to_string();
        self.rewrite(path, async move { inner.save(&p, request).await })
            .await
    }

    async fn delete(&self, path: &str) -> Result<()> {
        self.inner.delete(path).await
    }

    /// Not stamped here: [`Collab::rename`] moves the ids, and stamps the
    /// file at its new path then.
    async fn rename(&self, old_path: &str, new_path: &str) -> Result<ContentsModel> {
        self.inner.rename(old_path, new_path).await
    }

    async fn new_untitled(
        &self,
        parent_dir: &str,
        kind: ContentType,
        ext: Option<&str>,
    ) -> Result<ContentsModel> {
        let (collab, inner) = (self.collab.clone(), self.inner.clone());
        let (parent_dir, ext) = (parent_dir.to_string(), ext.map(str::to_string));
        detached(async move {
            // The move lock for reading and the directory claimed, so the
            // ids at the new path go with the file made, no rename or
            // delete lands, and nothing looks at a path under the
            // directory meanwhile. Lookups elsewhere go on.
            // A new folder's ids and grants go under its whole prefix, so
            // lookups inside it wait too; a new file is a direct child.
            let scope = match kind {
                ContentType::Directory => Scope::Subtree,
                _ => Scope::Children,
            };
            let _creating = collab.begin_create(&parent_dir, scope).await;
            let _writing = collab.stamps.begin_write(&[&parent_dir]).await;
            let model = inner
                .new_untitled(&parent_dir, kind, ext.as_deref())
                .await?;
            collab.forget_new_file(&model.path).await;
            Ok(model)
        })
        .await
    }

    /// A copy named by its full path overwrites a file that may have a
    /// grant, which then keeps it, unless the file was replaced outside.
    /// Runs to the end if the caller goes away, as [`Stamping::rewrite`]
    /// does, so a copy is never left unstamped. A copy over a file is
    /// reported to the write observer as a save is, for the destination;
    /// a copy into a directory makes a new file and is not.
    async fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel> {
        let (collab, inner) = (self.collab.clone(), self.inner.clone());
        let (from_path, to_dir) = (from_path.to_string(), to_dir.map(str::to_string));
        detached(async move {
            // A destination that isn't an existing directory is the file
            // copied over: known beforehand, so it is checked, marked and
            // restamped as a save is.
            let target = match &to_dir {
                Some(d) => {
                    let is_dir = inner
                        .get(d, GetOptions::default())
                        .await
                        .is_ok_and(|m| m.kind == ContentType::Directory);
                    (!is_dir).then(|| d.clone())
                }
                None => None,
            };
            if let Some(dest) = target {
                // The source too, so a rename of it waits for the copy.
                let _writing = collab.stamps.begin_write(&[&from_path, &dest]).await;
                let _turn = collab.stamps.turn(&dest).await;
                let seen = collab.check_for_write(&dest).await;
                let others = collab.check_others(&dest).await;
                let watched = collab.stamps.observe(&dest);
                let _saving = collab.stamps.begin(&dest);
                let model = inner.copy(&from_path, to_dir.as_deref()).await?;
                if let Err(e) = collab.restamp(&dest, &seen).await {
                    tracing::warn!("{e}");
                }
                collab.restamp_others(others).await;
                if let Some(watched) = watched {
                    watched.done();
                }
                return Ok(model);
            }
            // A new file is made in the directory: the move lock for
            // reading and the directory claimed, as `new_untitled` does,
            // so the ids at its path go with the file. With no directory
            // given, the copy goes beside its source.
            let normalized = normalize_api_path(&from_path);
            let dir = match &to_dir {
                Some(d) => d.as_str(),
                None => crate::folder::dir_of(&normalized),
            };
            // A copy is always of a file, so a direct child.
            let _creating = collab.begin_create(dir, Scope::Children).await;
            // The new file's name isn't known yet: the write is
            // registered on its directory. With none given, the copy goes
            // beside its source, and the source's registration covers
            // every rename that covers that directory.
            let mut paths = vec![from_path.as_str()];
            paths.extend(to_dir.as_deref());
            let _writing = collab.stamps.begin_write(&paths).await;
            let model = inner.copy(&from_path, to_dir.as_deref()).await?;
            // Whatever grant is
            // stored at its path is from a file that was deleted outside.
            collab.forget_new_file(&model.path).await;
            Ok(model)
        })
        .await
    }

    async fn list_checkpoints(&self, path: &str) -> Result<Vec<Checkpoint>> {
        self.inner.list_checkpoints(path).await
    }

    async fn create_checkpoint(&self, path: &str) -> Result<Checkpoint> {
        self.inner.create_checkpoint(path).await
    }

    async fn restore_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
        let inner = self.inner.clone();
        let (p, id) = (path.to_string(), checkpoint_id.to_string());
        self.rewrite(path, async move { inner.restore_checkpoint(&p, &id).await })
            .await
    }

    async fn delete_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
        self.inner.delete_checkpoint(path, checkpoint_id).await
    }

    async fn trust(&self, path: &str) -> Result<()> {
        self.inner.trust(path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_in_flight_hides_the_file_from_checks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let stamps = Arc::new(Stamps::new(Some(dir.path().to_path_buf())));
        let before = stamps.current("a.ipynb").expect("a stamp");
        assert_eq!(stamps.current("/a.ipynb"), Some(before.clone()));

        let guard = stamps.begin("a.ipynb");
        std::fs::write(dir.path().join("a.ipynb"), "{ }").unwrap();
        assert_eq!(stamps.current("a.ipynb"), None, "no stamp while saving");
        assert!(stamps.current("b.ipynb").is_none(), "no file, no stamp");
        drop(guard);
        let after = stamps.current("a.ipynb").expect("a stamp again");
        assert_ne!(after, before);
        assert!(!stamps.lock().paths.values().any(|p| p.in_flight > 0));
    }

    /// The guard clears on an error or a dropped future too, and nested
    /// saves of one path are counted.
    #[test]
    fn the_guard_clears_on_drop_and_counts() {
        let stamps = Arc::new(Stamps::new(None));
        let a = stamps.begin("x");
        let b = stamps.begin("x");
        drop(a);
        assert_eq!(stamps.lock().paths.get("x").map(|p| p.in_flight), Some(1));
        drop(b);
        assert!(!stamps.lock().paths.values().any(|p| p.in_flight > 0));
        assert_eq!(stamps.current("x"), None, "no root, no stamp");
    }

    /// A check is void if a save of its own path began or ended while it
    /// looked, and only then: a save of another path doesn't touch it.
    #[test]
    fn only_a_save_of_the_same_path_voids_a_check() {
        let stamps = Arc::new(Stamps::new(None));
        let mark = stamps.mark("a.ipynb").expect("no save of a in flight");
        drop(stamps.begin("b.ipynb"));
        assert!(
            stamps.stable_since("a.ipynb", mark),
            "b's save is no matter"
        );
        let guard = stamps.begin("a.ipynb");
        assert!(!stamps.stable_since("a.ipynb", mark), "a save began");
        assert!(stamps.mark("a.ipynb").is_none(), "and is in flight");
        drop(guard);
        assert!(!stamps.stable_since("a.ipynb", mark), "and ended");
        let again = stamps.mark("a.ipynb").unwrap();
        assert!(stamps.stable_since("a.ipynb", again));
    }

    /// Paths remembered once their saves are over are bounded, and a
    /// check that began before one was forgotten isn't called stable.
    #[test]
    fn forgetting_old_paths_voids_older_checks() {
        let stamps = Arc::new(Stamps::new(None));
        let mark = stamps.mark("a.ipynb").unwrap();
        for i in 0..=REMEMBERED {
            drop(stamps.begin(&format!("n{i}.ipynb")));
        }
        assert!(stamps.lock().paths.len() <= REMEMBERED);
        assert!(!stamps.stable_since("a.ipynb", mark));
    }

    /// The file is written, its restamp not yet done: a grant read keeps
    /// the grant, and once the save is over, the new stamp stands.
    #[tokio::test]
    async fn a_save_in_flight_does_not_drop_the_grant() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let (id, _) = collab.index("a.ipynb").await;
        collab
            .set_grant(id, Some(r#"{"run":[""]}"#.into()))
            .await
            .unwrap();
        assert!(collab.grant("a.ipynb").await.is_some());

        let seen = collab.check_for_write("a.ipynb").await;
        let saving = collab.stamps.begin("a.ipynb");
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 1 }").unwrap();
        assert!(collab.grant("a.ipynb").await.is_some(), "mid-save");
        collab.restamp("a.ipynb", &seen).await.unwrap();
        drop(saving);
        assert!(collab.grant("a.ipynb").await.is_some(), "after the restamp");

        // The same write with no save in flight is an outside change.
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 22 }").unwrap();
        assert_eq!(collab.grant("a.ipynb").await, None);
    }

    /// Saves through the wrapper keep the grant, whether they succeed
    /// or fail, and nothing stays marked in flight.
    #[tokio::test]
    async fn the_wrapper_restamps_and_clears_its_mark() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (id, _) = collab.index("a.ipynb").await;
        collab
            .set_grant(id, Some(r#"{"run":[""]}"#.into()))
            .await
            .unwrap();
        contents
            .save(
                "a.ipynb",
                SaveRequest::text("{\"saved\": true, \"by\": \"server\"}"),
            )
            .await
            .unwrap();
        assert!(collab.grant("a.ipynb").await.is_some());
        // A failed save: nothing changed, and the mark is gone.
        let bad = SaveRequest::text("x")
            .with_if_match(ganymede_contents::IfMatch::Hashes(vec!["nope".into()]));
        assert!(contents.save("a.ipynb", bad).await.is_err());
        assert!(!collab.stamps.lock().paths.values().any(|p| p.in_flight > 0));
        assert!(collab.grant("a.ipynb").await.is_some());
    }

    /// A copy named by its full path overwrites a file with a grant, which
    /// keeps it.
    #[tokio::test]
    async fn a_copy_over_a_file_restamps_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{\"from\": \"a longer source file\"}").unwrap();
        std::fs::write(root.join("b.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (id, _) = collab.index("b.ipynb").await;
        collab
            .set_grant(id, Some(r#"{"run":[""]}"#.into()))
            .await
            .unwrap();
        assert!(collab.grant("b.ipynb").await.is_some());
        let model = contents.copy("a.ipynb", Some("b.ipynb")).await.unwrap();
        assert_eq!(model.path, "b.ipynb");
        assert!(std::fs::read_to_string(root.join("b.ipynb"))
            .unwrap()
            .contains("longer"));
        assert!(collab.grant("b.ipynb").await.is_some());
    }

    /// A write after a check that left no grant standing (it was dropped,
    /// or the database wouldn't delete it) stamps nothing; one after a
    /// check that found the grant stamps it.
    #[tokio::test]
    async fn only_a_grant_that_stood_is_restamped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let (id, _) = collab.index("a.ipynb").await;
        collab
            .set_grant(id, Some(r#"{"run":[""]}"#.into()))
            .await
            .unwrap();
        let seen = collab.check_for_write("a.ipynb").await;
        assert!(seen.is_some());
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 1 }").unwrap();
        collab.restamp("a.ipynb", &None).await.unwrap();
        assert_eq!(collab.grant("a.ipynb").await, None, "nothing stamped");

        let (id, _) = collab.index("a.ipynb").await;
        collab
            .set_grant(id, Some(r#"{"run":[""]}"#.into()))
            .await
            .unwrap();
        let seen = collab.check_for_write("a.ipynb").await;
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 22 }").unwrap();
        collab.restamp("a.ipynb", &seen).await.unwrap();
        assert!(collab.grant("a.ipynb").await.is_some());
    }

    /// Saves of one path take turns, and the turn table empties.
    #[tokio::test]
    async fn saves_of_a_path_take_turns() {
        let stamps = Arc::new(Stamps::new(None));
        let first = stamps.turn("a.ipynb").await;
        let s2 = stamps.clone();
        let waiting = tokio::spawn(async move { drop(s2.turn("/a.ipynb/").await) });
        let s3 = stamps.clone();
        let second = tokio::spawn(async move { drop(s3.turn("a.ipynb").await) });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!second.is_finished(), "waits for the first");
        drop(first);
        second.await.unwrap();
        waiting.abort();
        assert!(stamps.turns.lock().unwrap().len() <= 1);
    }

    /// Grants are per (root, path): a server on a parent folder and one in
    /// a project directory share the database, and a save through either
    /// keeps both roots' grants on the file, but never blesses a file
    /// that changed outside.
    #[tokio::test]
    async fn a_save_keeps_the_grants_other_roots_hold() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        let proj = top.join("proj");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let db = top.join("ids.db");
        let at_top = Arc::new(Collab::new(
            ganymede_contents::FileIds::open(&db, &top),
            Some(top.clone()),
        ));
        let at_proj = Arc::new(Collab::new(
            ganymede_contents::FileIds::open(&db, &proj),
            Some(proj.clone()),
        ));
        let contents = at_top.stamping(Arc::new(
            ganymede_contents::FsContents::new(&top, Default::default()).unwrap(),
        ));
        let grant = r#"{"run":[""]}"#;
        let (id, _) = at_proj.index("a.ipynb").await;
        at_proj.set_grant(id, Some(grant.into())).await.unwrap();
        let (id, _) = at_top.index("proj/a.ipynb").await;
        at_top.set_grant(id, Some(grant.into())).await.unwrap();

        let save = |text: &'static str| {
            let contents = contents.clone();
            async move {
                contents
                    .save("proj/a.ipynb", SaveRequest::text(text))
                    .await
                    .unwrap();
            }
        };
        save("{\"saved\": \"through the parent root\"}").await;
        assert!(at_proj.grant("a.ipynb").await.is_some(), "project root's");
        assert!(at_top.grant("proj/a.ipynb").await.is_some(), "own root's");

        // Replaced outside, then saved through the server: neither
        // root's grant survives to bless the new file.
        std::fs::write(proj.join("a.ipynb"), "{\"replaced\": \"outside\"}").unwrap();
        save("{\"saved\": \"again, a longer one\"}").await;
        assert_eq!(at_proj.grant("a.ipynb").await, None);
        assert_eq!(at_top.grant("proj/a.ipynb").await, None);
    }

    /// A rename waits for a save in flight to finish, so the save's
    /// restamp isn't lost to the rename.
    #[tokio::test]
    async fn a_rename_waits_for_a_save_of_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let turn = collab.stamps.begin_write(&["a.ipynb"]).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "a.ipynb", "b.ipynb").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the save");
        drop(turn);
        rename.await.unwrap().unwrap();
        assert!(root.join("b.ipynb").is_file());
    }

    /// A rename of a directory waits for a save of a file under it, and
    /// a save that begins meanwhile waits for the rename.
    #[tokio::test]
    async fn a_rename_of_a_directory_waits_for_saves_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::write(root.join("d/a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        // A save of d/a.ipynb in flight: it holds its turn and a write.
        let writing = collab.stamps.begin_write(&["d/a.ipynb"]).await;
        let turn = collab.stamps.turn("d/a.ipynb").await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d", "e").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the save under d");
        // A save that begins now waits for the rename.
        let k = contents.clone();
        let late = tokio::spawn(async move {
            k.save("d/a.ipynb", SaveRequest::text("{\"late\": 1}"))
                .await
        });
        drop((turn, writing));
        rename.await.unwrap().unwrap();
        assert!(root.join("e/a.ipynb").is_file());
        // It ran after the rename: its path was gone, so it failed or made
        // the file anew, never wrote the renamed one before the rename.
        let _ = late.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("e/a.ipynb")).unwrap(),
            "{}"
        );
    }

    /// A root with `d/<name>` granted, and its contents.
    async fn granted_in_d(name: &str) -> (tempfile::TempDir, Arc<Collab>, Arc<dyn Contents>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::write(root.join("d").join(name), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (id, _) = collab.index(&format!("d/{name}")).await;
        collab.set_grant(id, Some(GRANT.into())).await.unwrap();
        (dir, collab, contents)
    }

    /// The production wrapper streams: it forwards `open_file` rather than
    /// taking the default `None`.
    #[tokio::test]
    async fn open_file_is_forwarded_to_the_inner_contents() {
        let (dir, _collab, contents) = granted_in_d("a.ipynb").await;
        std::fs::write(dir.path().join("d/data.bin"), [7u8; 123]).unwrap();
        let open = contents.open_file("d/data.bin").await.unwrap().unwrap();
        assert_eq!(open.len, 123);
        assert_eq!(open.name, "data.bin");
    }

    /// Waits until a write is registered, as `begin_write` does. Gives up
    /// after a second, and the caller's assertions then fail.
    async fn until_a_write_begins(collab: &Collab) {
        for _ in 0..500 {
            if collab.stamps.any_write() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    const SHORT: std::time::Duration = std::time::Duration::from_millis(50);
    const LONG: std::time::Duration = std::time::Duration::from_secs(10);

    /// A rename finishes while a save of an unrelated path is in flight.
    #[tokio::test]
    async fn a_rename_ignores_a_save_of_another_path() {
        let stamps = Arc::new(Stamps::new(None));
        let _other = stamps.begin_write(&["c.ipynb"]).await;
        tokio::time::timeout(LONG, stamps.begin_rename("a.ipynb", "b.ipynb"))
            .await
            .expect("not held up by c.ipynb");
    }

    /// A rename of `d` waits for a write under `d` and not for one of
    /// `dd/x.ipynb`, and a save of an unrelated path begins and ends
    /// meanwhile, which the old server-wide lock queued behind it.
    #[tokio::test]
    async fn a_waiting_rename_holds_back_only_its_paths() {
        let stamps = Arc::new(Stamps::new(None));
        let _sibling = stamps.begin_write(&["dd/x.ipynb"]).await;
        let under = stamps.begin_write(&["d/a.ipynb"]).await;
        let s = stamps.clone();
        let rename = tokio::spawn(async move { s.begin_rename("d", "e").await });
        tokio::time::sleep(SHORT).await;
        assert!(!rename.is_finished(), "waits for the save under d");
        tokio::time::timeout(LONG, stamps.begin_write(&["x.ipynb"]))
            .await
            .expect("an unrelated save is not queued");
        let s = stamps.clone();
        let late = tokio::spawn(async move { s.begin_write(&["e/new.ipynb"]).await });
        tokio::time::sleep(SHORT).await;
        assert!(!late.is_finished(), "a save under the new path waits");
        drop(under);
        let renaming = tokio::time::timeout(LONG, rename)
            .await
            .expect("the rename goes ahead")
            .unwrap();
        drop(renaming);
        tokio::time::timeout(LONG, late)
            .await
            .expect("the save goes ahead after the rename")
            .unwrap();
    }

    /// A rename dropped while it waits gives its paths back.
    #[tokio::test]
    async fn a_rename_dropped_while_waiting_unblocks_its_paths() {
        let stamps = Arc::new(Stamps::new(None));
        let held = stamps.begin_write(&["d/a.ipynb"]).await;
        let s = stamps.clone();
        let rename = tokio::spawn(async move { s.begin_rename("d", "e").await });
        tokio::time::sleep(SHORT).await;
        assert!(!rename.is_finished());
        assert!(
            tokio::time::timeout(SHORT, stamps.begin_write(&["d/b.ipynb"]))
                .await
                .is_err(),
            "held back while the rename waits"
        );
        rename.abort();
        let _ = rename.await;
        tokio::time::timeout(LONG, stamps.begin_write(&["d/b.ipynb"]))
            .await
            .expect("the block went with the rename");
        assert!(stamps.writes.none_claimed());
        drop(held);
    }

    /// Saves in flight don't count towards the remembered paths, so a
    /// cleanup that evicts nothing doesn't move the floor and looks at
    /// quiet files stay stable.
    #[test]
    fn many_saves_in_flight_do_not_unsettle_looks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let stamps = Arc::new(Stamps::new(Some(dir.path().to_path_buf())));
        let mut guards: Vec<_> = (0..REMEMBERED + 10)
            .map(|i| stamps.begin(&format!("n{i}.ipynb")))
            .collect();
        guards.truncate(REMEMBERED + 5);
        assert!(matches!(stamps.look("a.ipynb"), Look::Stamp(_)));
        assert_eq!(stamps.lock().floor, 0, "nothing was forgotten");
        drop(guards);
        assert!(matches!(stamps.look("a.ipynb"), Look::Stamp(_)));
    }

    /// A cleanup's floor is the latest end among the paths it dropped,
    /// not the end that triggered it, so a look begun after the older
    /// ends and before the last one is only made again if its own path
    /// was among those dropped.
    #[test]
    fn the_floor_is_the_latest_end_of_what_was_dropped() {
        let stamps = Arc::new(Stamps::new(None));
        for i in 0..REMEMBERED {
            drop(stamps.begin(&format!("n{i}.ipynb")));
        }
        assert_eq!(stamps.lock().floor, 0, "nothing dropped yet");
        let last_dropped = stamps.lock().paths["n255.ipynb"].last;
        drop(stamps.begin("trigger.ipynb"));
        let s = stamps.lock();
        assert_eq!(s.floor, last_dropped);
        assert!(s.floor < s.seq);
        assert_eq!(s.paths.len(), 1, "only the path that just ended stays");
        assert_eq!(s.quiet, 1);
    }

    /// A save of a path that was quiet takes it out of the quiet count.
    #[test]
    fn the_quiet_count_follows_saves() {
        let stamps = Arc::new(Stamps::new(None));
        drop(stamps.begin("x"));
        assert_eq!(stamps.lock().quiet, 1);
        let g = stamps.begin("x");
        assert_eq!(stamps.lock().quiet, 0);
        drop(g);
        assert_eq!(stamps.lock().quiet, 1);
    }

    /// A rename waits for a real `save` of its new path, which is
    /// registered while it waits for its turn.
    #[tokio::test]
    async fn a_rename_waits_for_a_real_save_of_its_new_path() {
        let (dir, collab, contents) = granted_in_d("a.ipynb").await;
        let root = dir.path().canonicalize().unwrap();
        let turn = collab.stamps.turn("b.ipynb").await;
        let k = contents.clone();
        let save = tokio::spawn(async move {
            k.save("b.ipynb", SaveRequest::text("{\"saved\": \"b\"}"))
                .await
        });
        until_a_write_begins(&collab).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d/a.ipynb", "b.ipynb").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the save of b.ipynb");
        drop(turn);
        save.await.unwrap().unwrap();
        // The save made b.ipynb first, so the rename onto it may fail; it
        // ran after the save either way.
        let _ = rename.await.unwrap();
        assert!(root.join("b.ipynb").is_file());
    }

    /// A rename of a directory waits for a real `save` under it, which
    /// is registered while it waits for its turn, and the save's
    /// restamp lands before the rename moves the grant.
    #[tokio::test]
    async fn a_rename_waits_for_a_real_save() {
        let (dir, collab, contents) = granted_in_d("a.ipynb").await;
        let root = dir.path().canonicalize().unwrap();
        let turn = collab.stamps.turn("d/a.ipynb").await;
        let k = contents.clone();
        let save = tokio::spawn(async move {
            k.save("d/a.ipynb", SaveRequest::text("{\"saved\": \"it\"}"))
                .await
        });
        until_a_write_begins(&collab).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d", "e").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the save");
        assert!(!save.is_finished(), "the save waits for its turn");
        drop(turn);
        save.await.unwrap().unwrap();
        rename.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("e/a.ipynb")).unwrap(),
            "{\"saved\": \"it\"}"
        );
        assert!(collab.grant("e/a.ipynb").await.is_some(), "held after");
    }

    /// A rename of a directory waits for a real `copy` into it, which
    /// is registered while it waits for its destination's turn.
    #[tokio::test]
    async fn a_rename_waits_for_a_real_copy() {
        let (dir, collab, contents) = granted_in_d("b.ipynb").await;
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("d/a.ipynb"), "{\"a\": 1}").unwrap();
        let turn = collab.stamps.turn("d/b.ipynb").await;
        let k = contents.clone();
        let copy = tokio::spawn(async move { k.copy("d/a.ipynb", Some("d/b.ipynb")).await });
        until_a_write_begins(&collab).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d", "e").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the copy");
        assert!(!copy.is_finished(), "the copy waits for its turn");
        drop(turn);
        copy.await.unwrap().unwrap();
        rename.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("e/b.ipynb")).unwrap(),
            "{\"a\": 1}"
        );
        assert!(collab.grant("e/b.ipynb").await.is_some(), "held after");
    }

    /// A rename of `d` waits for a real copy whose only registration under
    /// `d` is the destination, a file copied from outside it.
    #[tokio::test]
    async fn a_rename_waits_for_a_copy_onto_a_file_under_it() {
        let (dir, collab, contents) = granted_in_d("b.ipynb").await;
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{\"a\": 1}").unwrap();
        let turn = collab.stamps.turn("d/b.ipynb").await;
        let k = contents.clone();
        let copy = tokio::spawn(async move { k.copy("a.ipynb", Some("d/b.ipynb")).await });
        until_a_write_begins(&collab).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d", "e").await });
        tokio::time::sleep(SHORT).await;
        assert!(!rename.is_finished(), "waits for the copy");
        drop(turn);
        copy.await.unwrap().unwrap();
        rename.await.unwrap().unwrap();
        assert!(root.join("e/b.ipynb").is_file());
    }

    /// The same for a copy whose only registration under `d` is its
    /// source, a file copied onto one outside it.
    #[tokio::test]
    async fn a_rename_waits_for_a_copy_from_a_file_under_it() {
        let (dir, collab, contents) = granted_in_d("a.ipynb").await;
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("x")).unwrap();
        std::fs::write(root.join("x/b.ipynb"), "{}").unwrap();
        let turn = collab.stamps.turn("x/b.ipynb").await;
        let k = contents.clone();
        let copy = tokio::spawn(async move { k.copy("d/a.ipynb", Some("x/b.ipynb")).await });
        until_a_write_begins(&collab).await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d", "e").await });
        tokio::time::sleep(SHORT).await;
        assert!(!rename.is_finished(), "waits for the copy");
        drop(turn);
        copy.await.unwrap().unwrap();
        rename.await.unwrap().unwrap();
        assert!(root.join("e/a.ipynb").is_file());
    }

    /// A copy into a directory registers its source and, when one is
    /// given, the directory.
    #[tokio::test]
    async fn a_copy_into_a_directory_registers_its_source_and_directory() {
        for to_dir in [None, Some("x")] {
            let (dir, collab, contents, wrote, release) = gated(false).await;
            let root = dir.path().canonicalize().unwrap();
            std::fs::create_dir(root.join("d")).unwrap();
            std::fs::create_dir(root.join("x")).unwrap();
            std::fs::write(root.join("d/a.ipynb"), "{}").unwrap();
            let copy = tokio::spawn(async move { contents.copy("d/a.ipynb", to_dir).await });
            wrote.notified().await;
            assert!(
                tokio::time::timeout(SHORT, collab.stamps.begin_rename("d", "e"))
                    .await
                    .is_err(),
                "a rename of d waits for the copy ({to_dir:?})"
            );
            if let Some(dir) = to_dir {
                assert!(
                    tokio::time::timeout(SHORT, collab.stamps.begin_rename(dir, "y"))
                        .await
                        .is_err(),
                    "a rename of the directory copied into waits too"
                );
            }
            release.notify_one();
            copy.await.unwrap().unwrap();
            tokio::time::timeout(LONG, collab.stamps.begin_rename("d", "e"))
                .await
                .expect("free once the copy is done");
        }
    }

    /// Contents whose save, copy and new untitled file write the file, say
    /// so, and then wait to be let go; a save can fail instead.
    struct Gated {
        inner: Arc<dyn Contents>,
        wrote: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        fail: bool,
    }

    #[async_trait]
    impl Contents for Gated {
        async fn get(&self, path: &str, options: GetOptions) -> Result<ContentsModel> {
            self.inner.get(path, options).await
        }
        async fn save(&self, path: &str, request: SaveRequest) -> Result<ContentsModel> {
            let model = self.inner.save(path, request).await?;
            self.wrote.notify_one();
            self.release.notified().await;
            if self.fail {
                return Err(std::io::Error::other("failed after writing").into());
            }
            Ok(model)
        }
        async fn delete(&self, path: &str) -> Result<()> {
            self.inner.delete(path).await
        }
        async fn rename(&self, old_path: &str, new_path: &str) -> Result<ContentsModel> {
            self.inner.rename(old_path, new_path).await
        }
        async fn new_untitled(
            &self,
            parent_dir: &str,
            kind: ContentType,
            ext: Option<&str>,
        ) -> Result<ContentsModel> {
            let model = self.inner.new_untitled(parent_dir, kind, ext).await?;
            self.wrote.notify_one();
            self.release.notified().await;
            Ok(model)
        }
        async fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel> {
            let model = self.inner.copy(from_path, to_dir).await?;
            self.wrote.notify_one();
            self.release.notified().await;
            Ok(model)
        }
        async fn list_checkpoints(&self, path: &str) -> Result<Vec<Checkpoint>> {
            self.inner.list_checkpoints(path).await
        }
        async fn create_checkpoint(&self, path: &str) -> Result<Checkpoint> {
            self.inner.create_checkpoint(path).await
        }
        async fn restore_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
            self.inner.restore_checkpoint(path, checkpoint_id).await
        }
        async fn delete_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
            self.inner.delete_checkpoint(path, checkpoint_id).await
        }
    }

    /// A granted `a.ipynb`, and contents whose save is held open after
    /// it writes the file.
    async fn gated(
        fail: bool,
    ) -> (
        tempfile::TempDir,
        Arc<Collab>,
        Arc<dyn Contents>,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let (wrote, release) = (Arc::default(), Arc::default());
        let contents = collab.stamping(Arc::new(Gated {
            inner: Arc::new(ganymede_contents::FsContents::new(&root, Default::default()).unwrap()),
            wrote: Arc::clone(&wrote),
            release: Arc::clone(&release),
            fail,
        }));
        let (id, _) = collab.index("a.ipynb").await;
        collab.set_grant(id, Some(GRANT.into())).await.unwrap();
        (dir, collab, contents, wrote, release)
    }

    /// A real save is marked in flight from the check to the restamp: a
    /// grant read after the file is written, before the save ends, keeps
    /// the grant.
    #[tokio::test]
    async fn a_real_save_in_flight_does_not_drop_the_grant() {
        let (_dir, collab, contents, wrote, release) = gated(false).await;
        let save = tokio::spawn(async move {
            contents
                .save("a.ipynb", SaveRequest::text("{\"saved\": \"it\"}"))
                .await
        });
        wrote.notified().await;
        assert!(collab.grant("a.ipynb").await.is_some(), "mid-save");
        release.notify_one();
        save.await.unwrap().unwrap();
        assert!(collab.grant("a.ipynb").await.is_some(), "after the save");
    }

    /// A save that fails isn't restamped, whether or not it wrote: the
    /// grant is dropped on its next read, not blessed.
    #[tokio::test]
    async fn a_save_that_fails_is_not_restamped() {
        let (_dir, collab, contents, wrote, release) = gated(true).await;
        let save = tokio::spawn(async move {
            contents
                .save("a.ipynb", SaveRequest::text("{\"saved\": \"it\"}"))
                .await
        });
        wrote.notified().await;
        release.notify_one();
        assert!(save.await.unwrap().is_err());
        assert_eq!(collab.grant("a.ipynb").await, None, "not blessed");
    }

    /// A file deleted outside and made anew by a save through the server
    /// doesn't keep the grant of the old one.
    #[tokio::test]
    async fn a_save_over_a_file_deleted_outside_drops_the_grant() {
        let (dir, collab, contents) = granted_in_d("a.ipynb").await;
        std::fs::remove_file(dir.path().join("d/a.ipynb")).unwrap();
        contents
            .save("d/a.ipynb", SaveRequest::text("{\"made\": \"anew\"}"))
            .await
            .unwrap();
        assert_eq!(collab.grant("d/a.ipynb").await, None);
    }

    /// A copy over a file replaced outside doesn't bless it.
    #[tokio::test]
    async fn a_copy_over_a_file_replaced_outside_drops_the_grant() {
        let (dir, collab, contents) = granted_in_d("b.ipynb").await;
        std::fs::write(dir.path().join("d/a.ipynb"), "{\"a\": 1}").unwrap();
        std::fs::write(dir.path().join("d/b.ipynb"), "{\"replaced\": \"by git\"}").unwrap();
        contents.copy("d/a.ipynb", Some("d/b.ipynb")).await.unwrap();
        assert_eq!(collab.grant("d/b.ipynb").await, None);
    }

    /// A grant set while a save of its file is in flight isn't stamped
    /// with the file before the save, so the save doesn't cost it.
    #[tokio::test]
    async fn a_grant_set_during_a_save_survives_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let (id, _) = collab.index("a.ipynb").await;
        let saving = collab.stamps.begin("a.ipynb");
        collab.set_grant(id, Some(GRANT.into())).await.unwrap();
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 1 }").unwrap();
        drop(saving);
        assert!(collab.grant("a.ipynb").await.is_some());
    }

    /// A copy onto a file is marked in flight from the check to the
    /// restamp: a grant read after the file is written, before the copy
    /// ends, keeps the grant.
    #[tokio::test]
    async fn a_copy_in_flight_does_not_drop_the_grant() {
        let (dir, collab, contents, wrote, release) = gated(false).await;
        std::fs::write(dir.path().join("src.ipynb"), "{\"copied\": \"over\"}").unwrap();
        let copy = tokio::spawn(async move { contents.copy("src.ipynb", Some("a.ipynb")).await });
        wrote.notified().await;
        assert!(collab.grant("a.ipynb").await.is_some(), "mid-copy");
        release.notify_one();
        copy.await.unwrap().unwrap();
        assert!(collab.grant("a.ipynb").await.is_some(), "after the copy");
    }

    /// A save that meets another write's mark on its path can't say the
    /// file matched, so it stamps nothing: a file changed outside isn't
    /// blessed.
    #[tokio::test]
    async fn a_save_beside_another_write_does_not_bless_an_outside_change() {
        let (dir, collab, contents) = granted_in_d("a.ipynb").await;
        std::fs::write(dir.path().join("d/a.ipynb"), "{\"outside\": \"change\"}").unwrap();
        let mark = collab.stamps.begin("d/a.ipynb");
        contents
            .save("d/a.ipynb", SaveRequest::text("{\"saved\": \"it\"}"))
            .await
            .unwrap();
        drop(mark);
        assert_eq!(collab.grant("d/a.ipynb").await, None);
    }

    /// A new untitled file waits for a rename in progress, as saves do.
    #[tokio::test]
    async fn new_untitled_waits_for_a_rename() {
        let (_dir, collab, contents) = granted_in_d("a.ipynb").await;
        let renaming = collab.stamps.begin_rename("d", "e").await;
        let k = contents.clone();
        let made = tokio::spawn(async move {
            k.new_untitled("d", ContentType::Notebook, Some(".ipynb"))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!made.is_finished(), "waits for the rename");
        drop(renaming);
        made.await.unwrap().unwrap();
    }

    /// A new untitled file and a copy into a directory take the move lock
    /// for reading, so they wait for a rename or delete in progress.
    #[tokio::test]
    async fn a_create_waits_for_a_move() {
        for copy in [false, true] {
            let (_dir, collab, contents) = granted_in_d("a.ipynb").await;
            std::fs::create_dir(_dir.path().join("sub")).unwrap();
            let moving = collab.begin_move().await;
            let k = contents.clone();
            let made = tokio::spawn(async move {
                if copy {
                    k.copy("d/a.ipynb", Some("sub")).await
                } else {
                    k.new_untitled("d", ContentType::Notebook, Some(".ipynb"))
                        .await
                }
            });
            tokio::time::sleep(SHORT).await;
            assert!(!made.is_finished(), "waits for the move lock ({copy})");
            drop(moving);
            made.await.unwrap().unwrap();
        }
    }

    const NOTEBOOK: &str = r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;

    /// A create into `sub`, held after it made its file. `gated`'s root
    /// has a granted `a.ipynb`; `other/x.ipynb` is granted too, and a
    /// grant with no stamp is stored at the path the create will make,
    /// as a file deleted outside would leave.
    struct HeldCreate {
        _dir: tempfile::TempDir,
        collab: Arc<Collab>,
        contents: Arc<dyn Contents>,
        release: Arc<tokio::sync::Notify>,
        create: tokio::task::JoinHandle<Result<ContentsModel>>,
        /// The path the create makes.
        new_path: &'static str,
        stale_id: String,
        other_id: String,
    }

    impl HeldCreate {
        async fn new(copy: bool) -> Self {
            let (dir, collab, contents, wrote, release) = gated(false).await;
            let root = dir.path();
            std::fs::create_dir(root.join("sub")).unwrap();
            std::fs::create_dir(root.join("other")).unwrap();
            std::fs::write(root.join("other/x.ipynb"), NOTEBOOK).unwrap();
            let (other_id, _) = collab.index("other/x.ipynb").await;
            collab
                .set_grant(other_id.clone(), Some(GRANT.into()))
                .await
                .unwrap();
            let new_path = if copy {
                "sub/a.ipynb"
            } else {
                "sub/Untitled.ipynb"
            };
            let (stale_id, _) = collab.index(new_path).await;
            collab
                .set_grant(stale_id.clone(), Some(GRANT.into()))
                .await
                .unwrap();
            let k = contents.clone();
            let create = tokio::spawn(async move {
                if copy {
                    k.copy("a.ipynb", Some("sub")).await
                } else {
                    k.new_untitled("sub", ContentType::Notebook, Some(".ipynb"))
                        .await
                }
            });
            wrote.notified().await;
            HeldCreate {
                _dir: dir,
                collab,
                contents,
                release,
                create,
                new_path,
                stale_id,
                other_id,
            }
        }

        /// Lets the create end, and checks it made the path. Returns the
        /// temporary root, which the caller keeps until the tasks it
        /// spawned against the files have ended: dropping it deletes
        /// them, and a task the create held up runs after this returns
        /// (#849).
        #[must_use = "dropping the root deletes the files a waiting task reads"]
        async fn finish(self) -> tempfile::TempDir {
            self.release.notify_one();
            let model = self.create.await.unwrap().unwrap();
            assert_eq!(model.path, self.new_path);
            self._dir
        }
    }

    /// A command for a room that reports when it has loaded.
    fn room_cmd() -> crate::collab::Cmd {
        crate::collab::Cmd::With(Box::new(|_: &mut ganymede_documents::Room| {
            Default::default()
        }))
    }

    fn room_id(id: &str) -> String {
        format!("json:notebook:{id}")
    }

    /// While a copy or a new untitled file is made in `sub`, the lookups
    /// of files elsewhere don't wait: a grant read, a path-and-grant read,
    /// an index and a room open.
    #[tokio::test]
    async fn a_create_does_not_hold_up_lookups_elsewhere() {
        for copy in [true, false] {
            let held = HeldCreate::new(copy).await;
            let c = &held.collab;
            let what = if copy { "copy" } else { "new untitled" };
            assert!(tokio::time::timeout(LONG, c.grant("other/x.ipynb"))
                .await
                .unwrap_or_else(|_| panic!("a grant read waited for the {what}"))
                .is_some());
            let found = tokio::time::timeout(LONG, c.path_and_grant(&held.other_id))
                .await
                .unwrap_or_else(|_| panic!("a path-and-grant read waited for the {what}"));
            assert_eq!(found.unwrap().0, "other/x.ipynb");
            let indexed = tokio::time::timeout(
                LONG,
                c.index_existing(&held.contents, "other/x.ipynb", GetOptions::default()),
            )
            .await
            .unwrap_or_else(|_| panic!("an index waited for the {what}"))
            .unwrap();
            assert_eq!(indexed, (held.other_id.clone(), false));
            let room = tokio::time::timeout(
                LONG,
                crate::collab::send_to_notebook_room(
                    c,
                    &held.contents,
                    &room_id(&held.other_id),
                    room_cmd(),
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("a room open waited for the {what}"));
            assert!(room.is_some());
            let _dir = held.finish().await;
        }
    }

    /// A create holds the move lock and its directory until its new path
    /// is forgotten: a rename waits, and so does a grant read at the new
    /// path, which finds no stale grant.
    #[tokio::test]
    async fn a_create_holds_the_locks_until_it_is_done() {
        for copy in [true, false] {
            let held = HeldCreate::new(copy).await;
            let c = &held.collab;
            assert!(
                tokio::time::timeout(SHORT, c.begin_move()).await.is_err(),
                "the move lock is held ({copy})"
            );
            assert!(
                tokio::time::timeout(SHORT, c.stamps.begin_rename("sub", "sub2"))
                    .await
                    .is_err(),
                "the write is registered on the directory ({copy})"
            );
            let k = c.clone();
            let path = held.new_path;
            let read = tokio::spawn(async move { k.grant(path).await });
            tokio::time::sleep(SHORT).await;
            assert!(!read.is_finished(), "a grant read waits ({copy})");
            held.release.notify_one();
            let release_done = held.create;
            release_done.await.unwrap().unwrap();
            assert_eq!(read.await.unwrap(), None, "no stale grant ({copy})");
        }
    }

    /// A path-and-grant read of the id a create's path had before waits
    /// for it, and then finds the id gone, not its stale grant.
    #[tokio::test]
    async fn a_path_and_grant_read_at_the_new_path_waits_for_the_create() {
        for copy in [true, false] {
            let held = HeldCreate::new(copy).await;
            let (k, id) = (held.collab.clone(), held.stale_id.clone());
            let read = tokio::spawn(async move { k.path_and_grant(&id).await });
            tokio::time::sleep(SHORT).await;
            assert!(!read.is_finished(), "waits for the create ({copy})");
            let collab = held.collab.clone();
            let stale = held.stale_id.clone();
            let _dir = held.finish().await;
            assert_eq!(read.await.unwrap(), None, "the stale id is gone ({copy})");
            assert_eq!(collab.path_of(&stale).await, None);
        }
    }

    /// Indexing the new path waits for the create, and then gets a fresh
    /// id, not the stale one the create forgot.
    #[tokio::test]
    async fn indexing_the_new_path_waits_for_the_create() {
        for copy in [true, false] {
            let held = HeldCreate::new(copy).await;
            let (k, contents, path) = (held.collab.clone(), held.contents.clone(), held.new_path);
            let index = tokio::spawn(async move {
                k.index_existing(&contents, path, GetOptions::default())
                    .await
            });
            tokio::time::sleep(SHORT).await;
            assert!(!index.is_finished(), "waits for the create ({copy})");
            let stale = held.stale_id.clone();
            let _dir = held.finish().await;
            let (id, fresh) = index.await.unwrap().unwrap();
            assert!(fresh, "a new id ({copy})");
            assert_ne!(id, stale, "not the stale one ({copy})");
        }
    }

    /// Opening a room by the id a create's path had before waits for the
    /// create, and then finds the id gone: no room opens on the new file
    /// under the stale id.
    #[tokio::test]
    async fn a_room_open_at_the_new_path_waits_for_the_create() {
        for copy in [true, false] {
            let held = HeldCreate::new(copy).await;
            let (k, contents) = (held.collab.clone(), held.contents.clone());
            let room = room_id(&held.stale_id);
            let open = tokio::spawn(async move {
                crate::collab::send_to_notebook_room(&k, &contents, &room, room_cmd())
                    .await
                    .is_some()
            });
            tokio::time::sleep(SHORT).await;
            assert!(!open.is_finished(), "waits for the create ({copy})");
            assert_eq!(held.collab.open_rooms(), 0);
            let collab = held.collab.clone();
            let _dir = held.finish().await;
            assert!(!open.await.unwrap(), "no room for a gone id ({copy})");
            assert_eq!(collab.open_rooms(), 0, "no room opened ({copy})");
        }
    }

    /// A delete and a rename wait for a create in progress, which holds
    /// the move lock too.
    #[tokio::test]
    async fn a_delete_and_a_rename_wait_for_a_create() {
        for rename in [false, true] {
            let held = HeldCreate::new(true).await;
            let (k, contents) = (held.collab.clone(), held.contents.clone());
            let op = tokio::spawn(async move {
                if rename {
                    k.rename(&contents, "other/x.ipynb", "other/y.ipynb")
                        .await
                        .map(drop)
                } else {
                    k.delete(&contents, "other/x.ipynb").await
                }
            });
            tokio::time::sleep(SHORT).await;
            assert!(!op.is_finished(), "waits for the create (rename {rename})");
            assert!(held._dir.path().join("other/x.ipynb").exists());
            held.release.notify_one();
            held.create.await.unwrap().unwrap();
            op.await.unwrap().unwrap();
        }
    }

    /// A copy or a new untitled file in the root holds up no lookup of a
    /// path in a folder, though one of a top-level path waits.
    #[tokio::test]
    async fn a_create_in_the_root_does_not_hold_up_lookups_in_folders() {
        for (kind, new_path) in [
            ("copy with no directory", "a-Copy1.ipynb"),
            ("copy into the root", "a-Copy1.ipynb"),
            ("new untitled", "Untitled.ipynb"),
        ] {
            let (dir, collab, contents, wrote, release) = gated(false).await;
            std::fs::create_dir(dir.path().join("other")).unwrap();
            std::fs::write(dir.path().join("other/x.ipynb"), NOTEBOOK).unwrap();
            let (id, _) = collab.index("other/x.ipynb").await;
            collab
                .set_grant(id.clone(), Some(GRANT.into()))
                .await
                .unwrap();
            let k = contents.clone();
            let create = tokio::spawn(async move {
                match kind {
                    "copy with no directory" => k.copy("a.ipynb", None).await,
                    "copy into the root" => k.copy("a.ipynb", Some("")).await,
                    _ => {
                        k.new_untitled("", ContentType::Notebook, Some(".ipynb"))
                            .await
                    }
                }
            });
            wrote.notified().await;
            let c = &collab;
            assert!(tokio::time::timeout(LONG, c.grant("other/x.ipynb"))
                .await
                .unwrap_or_else(|_| panic!("a grant read waited ({kind})"))
                .is_some());
            let found = tokio::time::timeout(LONG, c.path_and_grant(&id))
                .await
                .unwrap_or_else(|_| panic!("a path-and-grant read waited ({kind})"));
            assert_eq!(found.unwrap().0, "other/x.ipynb");
            let indexed = tokio::time::timeout(
                LONG,
                c.index_existing(&contents, "other/x.ipynb", GetOptions::default()),
            )
            .await
            .unwrap_or_else(|_| panic!("an index waited ({kind})"))
            .unwrap();
            assert_eq!(indexed, (id.clone(), false));
            let room = tokio::time::timeout(
                LONG,
                crate::collab::send_to_notebook_room(c, &contents, &room_id(&id), room_cmd()),
            )
            .await
            .unwrap_or_else(|_| panic!("a room open waited ({kind})"));
            assert!(room.is_some());
            // A top-level path does wait.
            let (k, top) = (collab.clone(), "zzz.ipynb");
            let read = tokio::spawn(async move { k.grant(top).await });
            tokio::time::sleep(SHORT).await;
            assert!(!read.is_finished(), "a top-level read waits ({kind})");
            release.notify_one();
            let model = create.await.unwrap().unwrap();
            assert_eq!(model.path, new_path, "{kind}");
            read.await.unwrap();
        }
    }

    /// Overlapping creates in one directory don't starve a lookup parked
    /// behind the first: it goes ahead before the second create's claim
    /// lands, not after both are done.
    #[tokio::test]
    async fn overlapping_creates_do_not_starve_a_lookup() {
        let held = HeldCreate::new(true).await;
        let c = held.collab.clone();
        let read = tokio::spawn({
            let c = c.clone();
            async move { c.grant("sub/x.ipynb").await }
        });
        tokio::time::sleep(SHORT).await;
        assert!(!read.is_finished(), "parked behind the first create");
        let second = tokio::spawn({
            let k = held.contents.clone();
            async move { k.copy("a.ipynb", Some("sub")).await }
        });
        tokio::time::sleep(SHORT).await;
        // The first create ends; the second is in the gate or still waiting
        // for the lookup, which must not stay behind it.
        held.release.notify_one();
        tokio::time::timeout(LONG, read)
            .await
            .expect("the lookup goes ahead while the second create is held")
            .unwrap();
        held.release.notify_one();
        held.create.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
    }

    /// A new folder's contents are looked up inside it: a grant read there
    /// waits for the create, though it is not a direct child of `sub`,
    /// while lookups of paths elsewhere, `other` and the root's, go on.
    #[tokio::test]
    async fn a_new_folder_holds_back_lookups_inside_it() {
        let (dir, collab, contents, wrote, release) = gated(false).await;
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        std::fs::write(dir.path().join("other/x.ipynb"), NOTEBOOK).unwrap();
        let (id, _) = collab.index("other/x.ipynb").await;
        collab
            .set_grant(id.clone(), Some(GRANT.into()))
            .await
            .unwrap();
        let k = contents.clone();
        let create =
            tokio::spawn(async move { k.new_untitled("sub", ContentType::Directory, None).await });
        wrote.notified().await;
        let c = collab.clone();
        let read = tokio::spawn(async move { c.grant("sub/Untitled Folder/x.ipynb").await });
        tokio::time::sleep(SHORT).await;
        assert!(!read.is_finished(), "waits for the new folder");
        let c = &collab;
        assert!(tokio::time::timeout(LONG, c.grant("other/x.ipynb"))
            .await
            .expect("a grant read elsewhere waited")
            .is_some());
        assert_eq!(
            tokio::time::timeout(LONG, c.path_and_grant(&id))
                .await
                .expect("a path-and-grant read elsewhere waited")
                .unwrap()
                .0,
            "other/x.ipynb"
        );
        let indexed = tokio::time::timeout(
            LONG,
            c.index_existing(&contents, "other/x.ipynb", GetOptions::default()),
        )
        .await
        .expect("an index elsewhere waited")
        .unwrap();
        assert_eq!(indexed, (id.clone(), false));
        let room = tokio::time::timeout(
            LONG,
            crate::collab::send_to_notebook_room(c, &contents, &room_id(&id), room_cmd()),
        )
        .await
        .expect("a room open elsewhere waited");
        assert!(room.is_some());
        assert!(!read.is_finished(), "still waiting for the new folder");
        release.notify_one();
        let model = create.await.unwrap().unwrap();
        assert_eq!(model.path, "sub/Untitled Folder");
        assert_eq!(read.await.unwrap(), None);
    }

    /// A copy with no directory goes beside its source, and claims the
    /// source's directory: reads of other files directly in it wait,
    /// others don't. A source in the root claims the top-level paths only.
    #[tokio::test]
    async fn a_copy_beside_its_source_claims_the_sources_directory() {
        for (source, under, elsewhere) in [
            ("sub/b.ipynb", "sub/zzz.ipynb", "other/x.ipynb"),
            ("a.ipynb", "zzz.ipynb", "other/x.ipynb"),
        ] {
            let (dir, collab, contents, wrote, release) = gated(false).await;
            let root = dir.path();
            std::fs::create_dir(root.join("sub")).unwrap();
            std::fs::create_dir(root.join("other")).unwrap();
            std::fs::write(root.join("sub/b.ipynb"), NOTEBOOK).unwrap();
            let (id, _) = collab.index("other/x.ipynb").await;
            collab.set_grant(id, Some(GRANT.into())).await.unwrap();
            let k = contents.clone();
            let copy = tokio::spawn(async move { k.copy(source, None).await });
            wrote.notified().await;
            let c = collab.clone();
            let read = tokio::spawn(async move { c.grant(under).await });
            tokio::time::sleep(SHORT).await;
            assert!(!read.is_finished(), "a read beside {source} waits");
            tokio::time::timeout(LONG, collab.grant(elsewhere))
                .await
                .expect("a read in another directory goes on");
            release.notify_one();
            copy.await.unwrap().unwrap();
            read.await.unwrap();
        }
    }

    /// A path-and-grant read waits for the move lock, as a grant read does.
    #[tokio::test]
    async fn a_path_and_grant_read_takes_the_move_lock() {
        let (_dir, collab, _contents) = granted_in_d("a.ipynb").await;
        let (id, _) = collab.index("d/a.ipynb").await;
        let moving = collab.begin_move().await;
        let c = collab.clone();
        let read = tokio::spawn(async move { c.path_and_grant(&id).await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!read.is_finished(), "waits for the move lock");
        drop(moving);
        assert!(read.await.unwrap().is_some());
    }

    /// A rename holds the move lock, so no room opens or delete lands
    /// between the disk rename and the ids following.
    #[tokio::test]
    async fn a_rename_takes_the_move_lock() {
        let (_dir, collab, contents) = granted_in_d("a.ipynb").await;
        let moving = collab.begin_move().await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "d/a.ipynb", "d/b.ipynb").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the move lock");
        drop(moving);
        rename.await.unwrap().unwrap();
    }

    /// A look that a cleanup of the remembered paths overlapped is made
    /// again, and finds the quiet path stable.
    #[test]
    fn a_look_overlapped_by_a_cleanup_is_made_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let stamps = Arc::new(Stamps::new(Some(dir.path().to_path_buf())));
        let mut cleanups = 0;
        let look = stamps.look_with("a.ipynb", || {
            if cleanups == 0 {
                cleanups += 1;
                for i in 0..=REMEMBERED {
                    drop(stamps.begin(&format!("other/{i}.ipynb")));
                }
            }
        });
        assert!(matches!(look, Look::Stamp(_)));
        // A save of the path itself in that window still makes it unstable.
        let mut once = true;
        let mut held = None;
        let look = stamps.look_with("a.ipynb", || {
            if std::mem::take(&mut once) {
                held = Some(stamps.begin("a.ipynb"));
            }
        });
        assert!(matches!(look, Look::Unstable));
        drop(held);
    }

    /// A delete through a server on the parent folder drops the grant
    /// another root holds on the file.
    #[tokio::test]
    async fn a_delete_drops_the_grants_other_roots_hold() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::create_dir(proj.join("d")).unwrap();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        std::fs::write(proj.join("d/b.ipynb"), "{}").unwrap();
        for p in ["a.ipynb", "d/b.ipynb"] {
            let (id, _) = at_proj.index(p).await;
            at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        }
        at_top.delete(&contents, "proj/a.ipynb").await.unwrap();
        assert_eq!(at_proj.grant("a.ipynb").await, None, "file");
        assert!(at_proj.grant("d/b.ipynb").await.is_some(), "others stay");
        at_top.delete(&contents, "proj/d/b.ipynb").await.unwrap();
        assert_eq!(at_proj.grant("d/b.ipynb").await, None, "in a directory");
    }

    /// Two servers on one database: one on a parent folder, one in a
    /// project directory that holds the grants.
    fn two_roots() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Arc<Collab>,
        Arc<Collab>,
        Arc<dyn Contents>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        let proj = top.join("proj");
        std::fs::create_dir(&proj).unwrap();
        let db = top.join("ids.db");
        let at_top = Arc::new(Collab::new(
            ganymede_contents::FileIds::open(&db, &top),
            Some(top.clone()),
        ));
        let at_proj = Arc::new(Collab::new(
            ganymede_contents::FileIds::open(&db, &proj),
            Some(proj.clone()),
        ));
        let contents = at_top.stamping(Arc::new(
            ganymede_contents::FsContents::new(&top, Default::default()).unwrap(),
        ));
        (dir, proj, at_top, at_proj, contents)
    }

    const GRANT: &str = r#"{"run":[""]}"#;

    /// A copy over a file keeps the grant another root holds on it.
    #[tokio::test]
    async fn a_copy_keeps_the_grants_other_roots_hold() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{\"a\": 1}").unwrap();
        std::fs::write(proj.join("b.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("b.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        assert!(at_proj.grant("b.ipynb").await.is_some());
        contents
            .copy("proj/a.ipynb", Some("proj/b.ipynb"))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(proj.join("b.ipynb")).unwrap(),
            "{\"a\": 1}"
        );
        assert!(at_proj.grant("b.ipynb").await.is_some(), "proj root's");
        drop(at_top);
    }

    /// A copy onto a file another root's grant stands on, replaced
    /// outside, doesn't bless it.
    #[tokio::test]
    async fn a_copy_over_a_file_replaced_outside_drops_other_roots_grants() {
        let (_dir, proj, _at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{\"a\": 1}").unwrap();
        std::fs::write(proj.join("b.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("b.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        std::fs::write(proj.join("b.ipynb"), "{\"replaced\": \"by git\"}").unwrap();
        contents
            .copy("proj/a.ipynb", Some("proj/b.ipynb"))
            .await
            .unwrap();
        assert_eq!(at_proj.grant("b.ipynb").await, None);
    }

    /// A grant with no stamp on a file deleted outside isn't taken by the
    /// file a save makes there.
    #[tokio::test]
    async fn an_unstamped_grant_does_not_pass_to_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let collab = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (id, _) = collab.index("a.ipynb").await;
        // Set during a save, so it has no stamp.
        let saving = collab.stamps.begin("a.ipynb");
        collab.set_grant(id, Some(GRANT.into())).await.unwrap();
        drop(saving);
        std::fs::remove_file(root.join("a.ipynb")).unwrap();
        contents
            .save("a.ipynb", SaveRequest::text("{\"made\": \"anew\"}"))
            .await
            .unwrap();
        assert_eq!(collab.grant("a.ipynb").await, None);
    }

    /// A copy into a directory is a new file: a grant stored at its path
    /// by a file deleted outside doesn't pass to it, under this root or
    /// another.
    #[tokio::test]
    async fn a_copy_into_a_directory_inherits_no_grant() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{\"a\": 1}").unwrap();
        // Left by files that were deleted outside, with no stamp.
        for (c, p) in [(&at_top, "proj/a-Copy1.ipynb"), (&at_proj, "a-Copy1.ipynb")] {
            let (id, _) = c.index(p).await;
            c.set_grant(id, Some(GRANT.into())).await.unwrap();
        }
        let model = contents.copy("proj/a.ipynb", Some("proj")).await.unwrap();
        assert_eq!(model.path, "proj/a-Copy1.ipynb");
        assert_eq!(at_top.grant("proj/a-Copy1.ipynb").await, None, "own root");
        assert_eq!(at_proj.grant("a-Copy1.ipynb").await, None, "other root");
    }

    /// As a copy, for a new untitled file.
    #[tokio::test]
    async fn a_new_untitled_file_inherits_no_grant() {
        let (_dir, _proj, at_top, at_proj, contents) = two_roots();
        for (c, p) in [
            (&at_top, "proj/Untitled.ipynb"),
            (&at_proj, "Untitled.ipynb"),
        ] {
            let (id, _) = c.index(p).await;
            c.set_grant(id, Some(GRANT.into())).await.unwrap();
        }
        let model = contents
            .new_untitled("proj", ContentType::Notebook, Some(".ipynb"))
            .await
            .unwrap();
        assert_eq!(model.path, "proj/Untitled.ipynb");
        assert_eq!(at_top.grant("proj/Untitled.ipynb").await, None, "own root");
        assert_eq!(at_proj.grant("Untitled.ipynb").await, None, "other root");
    }

    /// A save through one root over a file that is gone drops the grant
    /// with no stamp that another root holds on it.
    #[tokio::test]
    async fn a_save_over_a_missing_file_drops_other_roots_unstamped_grants() {
        let (_dir, proj, _at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("a.ipynb").await;
        // Set during a save, so it has no stamp.
        let saving = at_proj.stamps.begin("a.ipynb");
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        drop(saving);
        std::fs::remove_file(proj.join("a.ipynb")).unwrap();
        contents
            .save("proj/a.ipynb", SaveRequest::text("{\"made\": \"anew\"}"))
            .await
            .unwrap();
        assert_eq!(at_proj.grant("a.ipynb").await, None);
    }

    /// A rename of a file onto itself keeps its grants, under this root
    /// and another.
    #[tokio::test]
    async fn a_rename_onto_itself_keeps_the_grants() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        for (c, p) in [(&at_top, "proj/a.ipynb"), (&at_proj, "a.ipynb")] {
            let (id, _) = c.index(p).await;
            c.set_grant(id, Some(GRANT.into())).await.unwrap();
        }
        let _ = at_top
            .rename(&contents, "proj/a.ipynb", "proj/a.ipynb")
            .await;
        assert!(at_top.grant("proj/a.ipynb").await.is_some(), "own root");
        assert!(at_proj.grant("a.ipynb").await.is_some(), "other root");
    }

    /// A file renamed onto a path another root holds a grant on, left by
    /// a file deleted outside, doesn't take the grant.
    #[tokio::test]
    async fn a_rename_onto_a_path_inherits_no_grant() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("x.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("x.ipynb").await;
        // Set during a save, so it has no stamp.
        let saving = at_proj.stamps.begin("x.ipynb");
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        drop(saving);
        std::fs::remove_file(proj.join("x.ipynb")).unwrap();
        std::fs::write(proj.parent().unwrap().join("y.ipynb"), "{}").unwrap();
        at_top
            .rename(&contents, "y.ipynb", "proj/x.ipynb")
            .await
            .unwrap();
        assert_eq!(at_proj.grant("x.ipynb").await, None);
    }

    /// A copy onto a named file that is gone inherits no grant either,
    /// under this root or another.
    #[tokio::test]
    async fn a_copy_onto_a_missing_file_inherits_no_grant() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{\"a\": 1}").unwrap();
        std::fs::write(proj.join("b.ipynb"), "{}").unwrap();
        for (c, p) in [(&at_top, "proj/b.ipynb"), (&at_proj, "b.ipynb")] {
            let (id, _) = c.index(p).await;
            // Set during a save, so it has no stamp.
            let saving = c.stamps.begin(p);
            c.set_grant(id, Some(GRANT.into())).await.unwrap();
            drop(saving);
        }
        std::fs::remove_file(proj.join("b.ipynb")).unwrap();
        contents
            .copy("proj/a.ipynb", Some("proj/b.ipynb"))
            .await
            .unwrap();
        assert_eq!(at_top.grant("proj/b.ipynb").await, None, "own root");
        assert_eq!(at_proj.grant("b.ipynb").await, None, "other root");
    }

    /// A rename out of a root drops the grants that root holds, as a
    /// delete does, and not leave them on a path that is gone.
    #[tokio::test]
    async fn a_rename_out_of_a_root_drops_its_grants() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("a.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        at_top
            .rename(&contents, "proj/a.ipynb", "out.ipynb")
            .await
            .unwrap();
        assert_eq!(at_proj.grant("a.ipynb").await, None);
    }

    /// A rename through a server on the parent folder moves the grant
    /// another root holds on the file.
    #[tokio::test]
    async fn a_rename_moves_the_grants_other_roots_hold() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("a.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        at_top
            .rename(&contents, "proj/a.ipynb", "proj/b.ipynb")
            .await
            .unwrap();
        assert!(at_proj.grant("b.ipynb").await.is_some(), "followed");
        assert_eq!(at_proj.grant("a.ipynb").await, None);
    }

    /// As a file, for a directory with the file in it.
    #[tokio::test]
    async fn a_directory_rename_moves_the_grants_under_it() {
        let (_dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::create_dir(proj.join("d")).unwrap();
        std::fs::write(proj.join("d/a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("d/a.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        at_top.rename(&contents, "proj/d", "proj/e").await.unwrap();
        assert!(at_proj.grant("e/a.ipynb").await.is_some(), "followed");
        assert_eq!(at_proj.grant("d/a.ipynb").await, None);
    }

    /// A rename of the project directory itself through a server on the
    /// parent moves the root, with its ids and grants, to the new name.
    #[tokio::test]
    async fn a_rename_of_a_root_directory_moves_its_grants() {
        let (dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("a.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        at_top.rename(&contents, "proj", "proj2").await.unwrap();
        let top = dir.path().canonicalize().unwrap();
        let moved = top.join("proj2");
        assert!(moved.join("a.ipynb").exists());
        let at_moved = Collab::new(
            ganymede_contents::FileIds::open(&top.join("ids.db"), &moved),
            Some(moved.clone()),
        );
        assert!(at_moved.grant("a.ipynb").await.is_some(), "followed");
        assert_eq!(at_proj.grant("a.ipynb").await, None, "none at the old root");
    }

    /// A delete of the project directory itself drops its grants: a file
    /// made anew there doesn't inherit them. The directory is empty by then
    /// (the contents API deletes only empty ones), so the file went outside.
    #[tokio::test]
    async fn a_delete_of_a_root_directory_drops_its_grants() {
        let (dir, proj, at_top, at_proj, contents) = two_roots();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let (id, _) = at_proj.index("a.ipynb").await;
        at_proj.set_grant(id, Some(GRANT.into())).await.unwrap();
        std::fs::remove_file(proj.join("a.ipynb")).unwrap();
        at_top.delete(&contents, "proj").await.unwrap();
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("a.ipynb"), "{}").unwrap();
        let db = dir.path().canonicalize().unwrap().join("ids.db");
        let again = Collab::new(
            ganymede_contents::FileIds::open(&db, &proj),
            Some(proj.clone()),
        );
        // No row is left for the path (an unstamped grant on one would take
        // the new file): the id is new, and so holds no grant.
        let (_, new) = again.index("a.ipynb").await;
        assert!(new, "a stale row is left");
        assert_eq!(again.grant("a.ipynb").await, None);
    }

    /// As a directory above the root.
    #[tokio::test]
    async fn a_delete_of_a_directory_above_a_root_drops_its_grants() {
        let (dir, _proj, at_top, _at_proj, contents) = two_roots();
        let top = dir.path().canonicalize().unwrap();
        let inner = top.join("outer/proj");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("a.ipynb"), "{}").unwrap();
        let db = top.join("ids.db");
        let at_inner = Collab::new(
            ganymede_contents::FileIds::open(&db, &inner),
            Some(inner.clone()),
        );
        let (id, _) = at_inner.index("a.ipynb").await;
        at_inner.set_grant(id, Some(GRANT.into())).await.unwrap();
        std::fs::remove_file(inner.join("a.ipynb")).unwrap();
        std::fs::remove_dir(&inner).unwrap();
        at_top.delete(&contents, "outer").await.unwrap();
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("a.ipynb"), "{}").unwrap();
        let again = Collab::new(
            ganymede_contents::FileIds::open(&db, &inner),
            Some(inner.clone()),
        );
        // No row is left for the path (an unstamped grant on one would take
        // the new file): the id is new, and so holds no grant.
        let (_, new) = again.index("a.ipynb").await;
        assert!(new, "a stale row is left");
        assert_eq!(again.grant("a.ipynb").await, None);
    }

    type Calls = Arc<Mutex<Vec<(PathBuf, Option<String>, Option<String>)>>>;

    /// A function for the observer that records its calls.
    fn recorder() -> (Arc<crate::write_observer::WriteObserverFn>, Calls) {
        let calls = Calls::default();
        let into = calls.clone();
        let f: Arc<crate::write_observer::WriteObserverFn> = Arc::new(move |p, b, a| {
            into.lock().unwrap().push((
                p.to_path_buf(),
                b.map(str::to_string),
                a.map(str::to_string),
            ));
        });
        (f, calls)
    }

    /// A root with `a.ipynb` and `b.ipynb`, and its contents with an
    /// observer that has a recorder set.
    fn observed() -> (
        tempfile::TempDir,
        PathBuf,
        Arc<Collab>,
        Arc<dyn Contents>,
        Arc<crate::write_observer::WriteObserver>,
        Calls,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        std::fs::write(root.join("b.ipynb"), "{\"b\": \"a longer file\"}").unwrap();
        let observer = Arc::new(crate::write_observer::WriteObserver::new());
        let collab = Arc::new(
            Collab::new(ganymede_contents::FileIds::in_memory(), Some(root.clone()))
                .with_write_observer(observer.clone()),
        );
        let contents = collab.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        let (f, calls) = recorder();
        observer.set(Some(f));
        (dir, root, collab, contents, observer, calls)
    }

    /// A save is reported once, after it, with the file's absolute path
    /// and its stamp before and after; a new file has none before.
    #[tokio::test]
    async fn a_save_is_reported_with_the_stamps_around_it() {
        let (_dir, root, _collab, contents, _observer, calls) = observed();
        let before = ganymede_contents::file_stamp(&root.join("a.ipynb")).unwrap();
        contents
            .save(
                "a.ipynb",
                SaveRequest::text("{\"saved\": \"by the server\"}"),
            )
            .await
            .unwrap();
        let after = ganymede_contents::file_stamp(&root.join("a.ipynb")).unwrap();
        assert_ne!(before, after);
        assert_eq!(
            *calls.lock().unwrap(),
            [(root.join("a.ipynb"), Some(before), Some(after))]
        );

        calls.lock().unwrap().clear();
        contents
            .save("new.ipynb", SaveRequest::text("{}"))
            .await
            .unwrap();
        let made = ganymede_contents::file_stamp(&root.join("new.ipynb")).unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            [(root.join("new.ipynb"), None, Some(made))]
        );
    }

    /// A copy over a file is reported for the destination. A copy into a
    /// directory, which makes a new file, and a rename are not.
    #[tokio::test]
    async fn a_copy_over_a_file_is_reported_and_other_writes_are_not() {
        let (_dir, root, _collab, contents, _observer, calls) = observed();
        let before = ganymede_contents::file_stamp(&root.join("b.ipynb")).unwrap();
        contents.copy("a.ipynb", Some("b.ipynb")).await.unwrap();
        let after = ganymede_contents::file_stamp(&root.join("b.ipynb")).unwrap();
        assert_ne!(before, after);
        assert_eq!(
            *calls.lock().unwrap(),
            [(root.join("b.ipynb"), Some(before), Some(after))]
        );

        calls.lock().unwrap().clear();
        std::fs::create_dir(root.join("d")).unwrap();
        contents.copy("a.ipynb", Some("d")).await.unwrap();
        contents.copy("a.ipynb", None).await.unwrap();
        contents.rename("b.ipynb", "c.ipynb").await.unwrap();
        contents
            .new_untitled("", ContentType::Notebook, Some(".ipynb"))
            .await
            .unwrap();
        contents.delete("c.ipynb").await.unwrap();
        assert_eq!(*calls.lock().unwrap(), []);
    }

    /// A save that fails, and one on contents that are not a local
    /// directory, call nothing.
    #[tokio::test]
    async fn a_failed_write_and_non_local_contents_are_not_reported() {
        let (dir, root, _collab, contents, observer, calls) = observed();
        let bad = SaveRequest::text("x")
            .with_if_match(ganymede_contents::IfMatch::Hashes(vec!["nope".into()]));
        assert!(contents.save("a.ipynb", bad).await.is_err());
        assert!(contents
            .copy("missing.ipynb", Some("b.ipynb"))
            .await
            .is_err());
        assert_eq!(*calls.lock().unwrap(), []);

        let remote = Arc::new(
            Collab::new(ganymede_contents::FileIds::in_memory(), None)
                .with_write_observer(observer.clone()),
        );
        let contents = remote.stamping(Arc::new(
            ganymede_contents::FsContents::new(&root, Default::default()).unwrap(),
        ));
        contents
            .save("a.ipynb", SaveRequest::text("{\"saved\": 1}"))
            .await
            .unwrap();
        contents.copy("b.ipynb", Some("a.ipynb")).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), []);
        drop(dir);
    }

    /// With no observer, or none set, writes go on; setting, replacing
    /// and clearing one takes effect for the writes after it.
    #[tokio::test]
    async fn the_observer_can_be_set_replaced_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.ipynb"), "{}").unwrap();
        let fs =
            || Arc::new(ganymede_contents::FsContents::new(&root, Default::default()).unwrap());
        let none = Arc::new(Collab::new(
            ganymede_contents::FileIds::in_memory(),
            Some(root.clone()),
        ));
        none.stamping(fs())
            .save("a.ipynb", SaveRequest::text("{\"no\": \"observer\"}"))
            .await
            .unwrap();

        let (_dir, _root, _collab, contents, observer, first) = observed();
        let save = |text: &'static str| {
            let contents = contents.clone();
            async move {
                contents
                    .save("a.ipynb", SaveRequest::text(text))
                    .await
                    .unwrap();
            }
        };
        save("{\"n\": 1}").await;
        assert_eq!(first.lock().unwrap().len(), 1);

        let (f, second) = recorder();
        observer.set(Some(f));
        save("{\"n\": 22}").await;
        assert_eq!(first.lock().unwrap().len(), 1, "replaced");
        assert_eq!(second.lock().unwrap().len(), 1);

        observer.set(None);
        save("{\"n\": 333}").await;
        assert_eq!(first.lock().unwrap().len(), 1);
        assert_eq!(second.lock().unwrap().len(), 1, "cleared");

        let (f, third) = recorder();
        observer.set(Some(f));
        save("{\"n\": 4444}").await;
        assert_eq!(third.lock().unwrap().len(), 1, "set again");
    }

    /// The function is called with no lock held, so it can replace the
    /// observer.
    #[tokio::test]
    async fn the_observer_may_replace_itself_when_called() {
        let (_dir, _root, _collab, contents, observer, _calls) = observed();
        let again = observer.clone();
        observer.set(Some(Arc::new(move |_, _, _| again.set(None))));
        contents
            .save("a.ipynb", SaveRequest::text("{\"n\": 1}"))
            .await
            .unwrap();
        assert!(!observer.is_set());
    }
}
