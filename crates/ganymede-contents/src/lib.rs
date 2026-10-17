//! Contents seam of Ganymede: files, directories and checkpoints.
//!
//! The wire model ([`ContentsModel`], [`Checkpoint`], [`SaveRequest`]) follows
//! `jupyter_server`'s `/api/contents` field for field, so an HTTP layer can
//! serialize it straight to JupyterLab, VS Code or any other client of that
//! API. [`Contents`] is the trait those clients are served from;
//! [`FsContents`] is the local-filesystem implementation (sketch section 7.2:
//! "local filesystem with atomic writes and mtime checks").
//!
//! Design points, all from the iPad use case in sketch section 6, where the
//! notebook root lives in iCloud Drive and other writers touch the same files:
//!
//! - Files are the source of truth. Nothing is cached; every call reads the
//!   disk.
//! - Writes are atomic: temp file in the same directory, fsync, rename over
//!   the target. A sync client never sees a half-written notebook.
//! - External-change detection: a [`SaveRequest::last_modified`] older than
//!   the file's current mtime by more than 500 ms is a [`Error::Conflict`].
//! - The checkpoint directory can be moved outside the root, so checkpoints
//!   are not synced along with the notebooks.
//!
//! Paths in this API are "API paths": forward-slash separated, relative to
//! the root, no leading slash, `""` for the root itself, exactly as they
//! appear after `/api/contents/`.

mod fs;
mod model;
mod paths;

use async_trait::async_trait;

pub use fs::{FsContents, FsOptions, CHECKPOINT_ID, DEFAULT_CHECKPOINT_DIR};
pub use model::{Checkpoint, ContentType, ContentsModel, Format, GetOptions, SaveRequest};
pub use paths::normalize_api_path;

/// Errors from a [`Contents`] implementation. [`Error::status`] gives the
/// HTTP status `jupyter_server` would answer with, so the HTTP layer needs no
/// knowledge of the variants.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The path does not exist (or is hidden and `allow_hidden` is off).
    #[error("no such file or directory: {0}")]
    NotFound(String),
    /// The path escapes the root, the root is read-only, or the OS refused.
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The target already exists, or the file changed on disk after the
    /// client's copy was taken.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The request is malformed for the target: bad content, bad format,
    /// wrong type for a path, a non-empty directory, a hidden path on write.
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("notebook error: {0}")]
    Nbformat(#[from] ganymede_nbformat::Error),
}

impl Error {
    /// The HTTP status `jupyter_server` uses for this error.
    ///
    /// `Io`, `Json` and `Nbformat` are 500 (unexpected), except that an I/O
    /// error whose kind is `NotFound`, `PermissionDenied` or `AlreadyExists`
    /// maps to 404, 403 or 409, since those are the same conditions the
    /// explicit variants describe and can arise from races on disk.
    pub fn status(&self) -> u16 {
        use std::io::ErrorKind;
        match self {
            Error::NotFound(_) => 404,
            Error::Forbidden(_) => 403,
            Error::Conflict(_) => 409,
            Error::BadRequest(_) => 400,
            Error::Io(e) => match e.kind() {
                ErrorKind::NotFound => 404,
                ErrorKind::PermissionDenied => 403,
                ErrorKind::AlreadyExists => 409,
                _ => 500,
            },
            Error::Json(_) | Error::Nbformat(_) => 500,
        }
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Files, directories and checkpoints, as `jupyter_server`'s
/// `ContentsManager` exposes them.
///
/// Every `path` argument is an API path (see the crate docs). Implementations
/// must reject paths that escape their root with [`Error::Forbidden`].
#[async_trait]
pub trait Contents: Send + Sync {
    /// Model of a file, notebook or directory; `GET /api/contents/{path}`.
    ///
    /// With `options.content` the model carries the content: the notebook
    /// JSON for notebooks, a string for files (`text` or `base64` per
    /// `options.format`, or auto-detected), an array of child models without
    /// content for directories. `options.type_hint` forces the kind, as the
    /// `?type=` query parameter does.
    async fn get(&self, path: &str, options: GetOptions) -> Result<ContentsModel>;

    /// Create or overwrite; `PUT /api/contents/{path}`.
    ///
    /// Returns the saved model without content. See [`SaveRequest`] for the
    /// conflict check driven by `last_modified`.
    async fn save(&self, path: &str, request: SaveRequest) -> Result<ContentsModel>;

    /// Delete a file, notebook or empty directory, and its checkpoints;
    /// `DELETE /api/contents/{path}`.
    async fn delete(&self, path: &str) -> Result<()>;

    /// Move `old_path` to `new_path`, carrying checkpoints along;
    /// `PATCH /api/contents/{path}` with `{"path": new}`. Returns the model
    /// of the new path without content. [`Error::Conflict`] if `new_path`
    /// exists.
    async fn rename(&self, old_path: &str, new_path: &str) -> Result<ContentsModel>;

    /// Create `Untitled.ipynb`, `untitled<ext>` or `Untitled Folder` (with
    /// `jupyter_server`'s numbering: `Untitled1.ipynb`, `Untitled Folder 1`)
    /// inside `parent_dir`; `POST /api/contents/{dir}` with `{"type": ...}`.
    /// Returns the model without content.
    async fn new_untitled(
        &self,
        parent_dir: &str,
        kind: ContentType,
        ext: Option<&str>,
    ) -> Result<ContentsModel>;

    /// Copy a file or notebook; `POST /api/contents/{dir}` with
    /// `{"copy_from": from_path}`.
    ///
    /// `to_dir` is the destination directory (`None` means the source's own
    /// directory); the copy is named `<stem>-Copy1<ext>`, `-Copy2`, and so on,
    /// after stripping any existing `-CopyN` from the source name, as
    /// `jupyter_server` does. If `to_dir` names something that is not an
    /// existing directory it is taken as the full destination path and
    /// overwritten, which is also `jupyter_server`'s behaviour.
    async fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel>;

    /// `GET /api/contents/{path}/checkpoints`.
    async fn list_checkpoints(&self, path: &str) -> Result<Vec<Checkpoint>>;

    /// `POST /api/contents/{path}/checkpoints`. Snapshots the current file.
    async fn create_checkpoint(&self, path: &str) -> Result<Checkpoint>;

    /// `POST /api/contents/{path}/checkpoints/{id}`. Overwrites the file with
    /// the checkpoint's bytes.
    async fn restore_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()>;

    /// `DELETE /api/contents/{path}/checkpoints/{id}`.
    async fn delete_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_match_jupyter_server() {
        assert_eq!(Error::NotFound("x".into()).status(), 404);
        assert_eq!(Error::Forbidden("x".into()).status(), 403);
        assert_eq!(Error::Conflict("x".into()).status(), 409);
        assert_eq!(Error::BadRequest("x".into()).status(), 400);
        assert_eq!(Error::Io(std::io::Error::other("x")).status(), 500);
        assert_eq!(
            Error::Io(std::io::Error::from(std::io::ErrorKind::NotFound)).status(),
            404
        );
        let json_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        assert_eq!(Error::Json(json_err).status(), 500);
        assert_eq!(
            Error::Nbformat(ganymede_nbformat::Error::NotAnObject).status(),
            500
        );
    }
}
