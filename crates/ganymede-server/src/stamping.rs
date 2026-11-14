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
}

#[derive(Default)]
struct Saving {
    /// Saves in flight, by API path.
    paths: HashMap<String, usize>,
    /// Bumped whenever one begins or ends.
    epoch: u64,
}

/// A save of one path in flight, until dropped, however the save ends.
pub(crate) struct SaveGuard {
    stamps: Arc<Stamps>,
    path: String,
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        let mut s = self.stamps.lock();
        s.epoch += 1;
        if let Some(n) = s.paths.get_mut(&self.path) {
            *n -= 1;
            if *n == 0 {
                s.paths.remove(&self.path);
            }
        }
    }
}

impl Stamps {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Stamps {
            root,
            saving: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Saving> {
        self.saving.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The server is about to write the file at `path`.
    pub(crate) fn begin(self: &Arc<Self>, path: &str) -> SaveGuard {
        let path = normalize_api_path(path);
        let mut s = self.lock();
        s.epoch += 1;
        *s.paths.entry(path.clone()).or_default() += 1;
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

    /// The stamp of the file at `path` to check a grant against: `None`
    /// (keep the grant) when it can't be known, or when the server is
    /// writing the file, or began or finished a save while this looked.
    pub(crate) fn current(&self, path: &str) -> Option<String> {
        let path = normalize_api_path(path);
        let before = {
            let s = self.lock();
            if s.paths.contains_key(&path) {
                return None;
            }
            s.epoch
        };
        let stamp = self.own(&path)?;
        (self.lock().epoch == before).then_some(stamp)
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
            collab.check_grant_at(&path).await;
            let _saving = collab.stamps.begin(&path);
            let out = write.await?;
            if let Err(e) = collab.restamp(&path).await {
                tracing::warn!("{e}");
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
        if let Err(e) = self.collab.restamp(&model.path).await {
            tracing::warn!("{e}");
        }
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
        assert!(stamps.lock().paths.is_empty());
    }

    /// The guard clears on an error or a dropped future too, and nested
    /// saves of one path are counted.
    #[test]
    fn the_guard_clears_on_drop_and_counts() {
        let stamps = Arc::new(Stamps::new(None));
        let a = stamps.begin("x");
        let b = stamps.begin("x");
        drop(a);
        assert_eq!(stamps.lock().paths.get("x"), Some(&1));
        drop(b);
        assert!(stamps.lock().paths.is_empty());
        assert_eq!(stamps.current("x"), None, "no root, no stamp");
    }

    /// A save that begins and ends while a check looks at the file
    /// leaves the check without a stamp: the grant is kept.
    #[test]
    fn a_save_during_a_check_voids_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.ipynb"), "{}").unwrap();
        let stamps = Arc::new(Stamps::new(Some(dir.path().to_path_buf())));
        let before = stamps.lock().epoch;
        drop(stamps.begin("a.ipynb"));
        assert_ne!(stamps.lock().epoch, before);
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
        collab.restamp("a.ipynb").await.unwrap();
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
        assert!(collab.stamps.lock().paths.is_empty());
        assert!(collab.grant("a.ipynb").await.is_some());
    }
}
