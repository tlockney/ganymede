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
//! in flight by path, and a check that overlaps one, or that sees any
//! save begin or end while it looks at the file, keeps the grant.

use crate::collab::{detached, Collab};
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
    /// `None` when the contents are not a local directory: nothing is
    /// stamped or checked.
    root: Option<PathBuf>,
    saving: Mutex<Saving>,
    turns: Turns,
}

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
    /// Counts every begin and end, of any path.
    seq: u64,
    /// Paths dropped from `paths` to keep it small are forgotten as of
    /// here: a check that began before this can't say its path was
    /// quiet.
    floor: u64,
}

#[derive(Default)]
struct PathSaves {
    in_flight: usize,
    /// `seq` at this path's last begin or end.
    last: u64,
}

/// The most paths remembered once their saves are over.
const REMEMBERED: usize = 256;

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
        }
        if s.paths.len() > REMEMBERED {
            s.paths.retain(|_, p| p.in_flight > 0);
            s.floor = seq;
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
        }
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
        let p = s.paths.entry(path.clone()).or_default();
        p.in_flight += 1;
        p.last = seq;
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
        let path = normalize_api_path(path);
        let Some(mark) = self.mark(&path) else {
            return Look::Unstable;
        };
        let Some(stamp) = self.own(&path) else {
            return Look::Missing;
        };
        if self.stable_since(&path, mark) {
            Look::Stamp(stamp)
        } else {
            Look::Unstable
        }
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
    /// caller goes away, so a write is never left unstamped.
    async fn rewrite<T: Send + 'static>(
        &self,
        path: &str,
        write: impl std::future::Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let collab = self.collab.clone();
        let path = path.to_string();
        detached(async move {
            // One save of a path at a time, so the stamp the check saw is
            // the one this save's restamp compares with.
            let _turn = collab.stamps.turn(&path).await;
            let seen = collab.check_for_write(&path).await;
            // The same file's grants under other roots in the shared
            // database get the same check and restamp.
            let others = collab.check_others(&path).await;
            let _saving = collab.stamps.begin(&path);
            let out = write.await?;
            if let Err(e) = collab.restamp(&path, &seen).await {
                tracing::warn!("{e}");
            }
            collab.restamp_others(others).await;
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
        self.inner.new_untitled(parent_dir, kind, ext).await
    }

    /// A copy named by its full path overwrites a file that may have a
    /// grant, which then keeps it.
    async fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel> {
        let model = self.inner.copy(from_path, to_dir).await?;
        // The destination isn't known until the copy is made, so only its
        // restamp takes the path's turn and is marked in flight.
        let _turn = self.collab.stamps.turn(&model.path).await;
        let _saving = self.collab.stamps.begin(&model.path);
        if let Err(e) = self.collab.restamp_any(&model.path).await {
            tracing::warn!("{e}");
        }
        self.collab.restamp_others_unchecked(&model.path).await;
        Ok(model)
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

        let saving = collab.stamps.begin("a.ipynb");
        std::fs::write(root.join("a.ipynb"), "{ \"written\": 1 }").unwrap();
        assert!(collab.grant("a.ipynb").await.is_some(), "mid-save");
        collab.restamp_any("a.ipynb").await.unwrap();
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

    /// A rename waits for a save of either path to finish, so the save's
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
        let turn = collab.stamps.turn("a.ipynb").await;
        let (c, k) = (collab.clone(), contents.clone());
        let rename = tokio::spawn(async move { c.rename(&k, "a.ipynb", "b.ipynb").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!rename.is_finished(), "waits for the save");
        drop(turn);
        rename.await.unwrap().unwrap();
        assert!(root.join("b.ipynb").is_file());
    }
}
