//! Local-filesystem [`Contents`].

use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use jcore_nbformat::Notebook;
use serde_json::Value;

use crate::model::{Checkpoint, ContentType, ContentsModel, Format, GetOptions, SaveRequest};
use crate::paths::ApiPath;
use crate::{Contents, Error, Result};

/// The one checkpoint id `jupyter_server`'s `FileCheckpoints` uses.
pub const CHECKPOINT_ID: &str = "checkpoint";
/// Name of the per-directory checkpoint folder when no
/// [`FsOptions::checkpoint_dir`] is configured.
pub const DEFAULT_CHECKPOINT_DIR: &str = ".ipynb_checkpoints";

/// How much newer than the client's `last_modified` the file on disk may be
/// before a save is a conflict.
const CONFLICT_WINDOW: TimeDelta = TimeDelta::milliseconds(500);

/// Lenient base64 decoder for uploads: `jupyter_server` uses
/// `base64.decodebytes`, which ignores padding problems and whitespace.
const BASE64_LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Options for [`FsContents`].
#[derive(Debug, Clone, Default)]
pub struct FsOptions {
    /// Where checkpoints live.
    ///
    /// `None` (the default) is `jupyter_server`'s layout: the checkpoint of
    /// `dir/name.ext` is `<root>/dir/.ipynb_checkpoints/name-checkpoint.ext`.
    ///
    /// `Some(dir)` keeps checkpoints out of the root, which matters when the
    /// root is an iCloud-synced folder: the checkpoint of `dir/name.ext` is
    /// then `<dir>/dir/name-checkpoint.ext`, mirroring the root's layout so
    /// that files of the same name in different directories do not collide.
    pub checkpoint_dir: Option<PathBuf>,
    /// Serve paths with a component starting with `.`. Off by default: such
    /// paths are 404 on read and 400 on write, and skipped in listings.
    pub allow_hidden: bool,
    /// Refuse every mutation with `Forbidden` and report `writable: false`.
    pub read_only: bool,
}

/// [`Contents`] over a directory on the local filesystem.
///
/// All I/O is `std::fs` on a `spawn_blocking` thread. Every call starts from
/// the disk; nothing is cached, so files changed underneath the server (by
/// iCloud, an editor, another client) are seen on the next request.
#[derive(Debug, Clone)]
pub struct FsContents {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Canonical root; everything served resolves under it.
    root: PathBuf,
    options: FsOptions,
}

impl FsContents {
    /// Serve `root_dir`, which must exist and be a directory. The root is
    /// canonicalized so that symlink checks compare like with like.
    pub fn new(root_dir: impl AsRef<Path>, options: FsOptions) -> Result<Self> {
        let root = root_dir.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("contents root is not a directory: {}", root.display()),
            )));
        }
        Ok(Self {
            inner: Arc::new(Inner { root, options }),
        })
    }

    /// The canonical root directory.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn options(&self) -> &FsOptions {
        &self.inner.options
    }

    async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Inner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || f(&inner))
            .await
            .map_err(|e| Error::Io(io::Error::other(e)))?
    }
}

#[async_trait]
impl Contents for FsContents {
    async fn get(&self, path: &str, options: GetOptions) -> Result<ContentsModel> {
        let path = path.to_owned();
        self.run(move |i| i.get(&path, options)).await
    }

    async fn save(&self, path: &str, request: SaveRequest) -> Result<ContentsModel> {
        let path = path.to_owned();
        self.run(move |i| i.save(&path, request)).await
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let path = path.to_owned();
        self.run(move |i| i.delete(&path)).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<ContentsModel> {
        let (old_path, new_path) = (old_path.to_owned(), new_path.to_owned());
        self.run(move |i| i.rename(&old_path, &new_path)).await
    }

    async fn new_untitled(
        &self,
        parent_dir: &str,
        kind: ContentType,
        ext: Option<&str>,
    ) -> Result<ContentsModel> {
        let parent_dir = parent_dir.to_owned();
        let ext = ext.map(str::to_owned);
        self.run(move |i| i.new_untitled(&parent_dir, kind, ext.as_deref()))
            .await
    }

    async fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel> {
        let from_path = from_path.to_owned();
        let to_dir = to_dir.map(str::to_owned);
        self.run(move |i| i.copy(&from_path, to_dir.as_deref()))
            .await
    }

    async fn list_checkpoints(&self, path: &str) -> Result<Vec<Checkpoint>> {
        let path = path.to_owned();
        self.run(move |i| i.list_checkpoints(&path)).await
    }

    async fn create_checkpoint(&self, path: &str) -> Result<Checkpoint> {
        let path = path.to_owned();
        self.run(move |i| i.create_checkpoint(&path)).await
    }

    async fn restore_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
        let (path, id) = (path.to_owned(), checkpoint_id.to_owned());
        self.run(move |i| i.restore_checkpoint(&path, &id)).await
    }

    async fn delete_checkpoint(&self, path: &str, checkpoint_id: &str) -> Result<()> {
        let (path, id) = (path.to_owned(), checkpoint_id.to_owned());
        self.run(move |i| i.delete_checkpoint(&path, &id)).await
    }
}

/// A validated API path together with its location on disk.
#[derive(Debug, Clone)]
struct Resolved {
    api: ApiPath,
    os: PathBuf,
}

// Path resolution and safety.
impl Inner {
    /// Validate `path` and check that nothing on disk (a symlink) makes it
    /// resolve outside the root. Does not apply the hidden-path policy.
    fn resolve(&self, path: &str) -> Result<Resolved> {
        let api = ApiPath::parse(path)?;
        let os = api.os_path(&self.root);
        self.check_inside_root(&os)?;
        Ok(Resolved { api, os })
    }

    /// [`Inner::resolve`] plus the read-side hidden policy: hidden paths are
    /// `NotFound`, as `jupyter_server` answers a GET.
    fn resolve_for_read(&self, path: &str) -> Result<Resolved> {
        let r = self.resolve(path)?;
        if r.api.is_hidden() && !self.options.allow_hidden {
            return Err(Error::NotFound(r.api.to_string()));
        }
        Ok(r)
    }

    /// [`Inner::resolve`] plus the write-side policies: `Forbidden` when the
    /// root is read-only, `BadRequest` for hidden paths (as `jupyter_server`
    /// answers a save, delete or rename of one).
    fn resolve_for_write(&self, path: &str) -> Result<Resolved> {
        if self.options.read_only {
            return Err(Error::Forbidden("contents root is read-only".into()));
        }
        let r = self.resolve(path)?;
        if r.api.is_hidden() && !self.options.allow_hidden {
            return Err(Error::BadRequest(format!(
                "Cannot create, modify or delete hidden file or directory: {}",
                r.api
            )));
        }
        Ok(r)
    }

    /// `os` was built from the canonical root and validated components, so
    /// the only way it can point outside the root is through a symlink.
    /// Canonicalize the deepest existing ancestor (the path itself if it
    /// exists) and check it is under the root. A dangling symlink is
    /// rejected too: writing through it would create a file at its target.
    fn check_inside_root(&self, os: &Path) -> Result<()> {
        let escape = || Error::Forbidden(format!("{} is outside the contents root", os.display()));
        let mut p = os;
        loop {
            match fs::symlink_metadata(p) {
                Ok(_) => {
                    let canonical = p.canonicalize().map_err(|e| {
                        if e.kind() == io::ErrorKind::NotFound {
                            escape()
                        } else {
                            Error::Io(e)
                        }
                    })?;
                    return if canonical.starts_with(&self.root) {
                        Ok(())
                    } else {
                        Err(escape())
                    };
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    p = p.parent().ok_or_else(escape)?;
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    fn stat(&self, r: &Resolved) -> Result<Metadata> {
        fs::metadata(&r.os).map_err(|e| io_to_error(e, &r.api))
    }
}

// Models.
impl Inner {
    fn base_model(&self, r: &Resolved, meta: &Metadata, kind: ContentType) -> ContentsModel {
        ContentsModel {
            name: r.api.name().to_owned(),
            path: r.api.to_string(),
            kind,
            created: format_time(created_time(meta)),
            last_modified: format_time(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)),
            mimetype: None,
            format: None,
            content: None,
            writable: !self.options.read_only && !meta.permissions().readonly(),
            size: (kind != ContentType::Directory).then_some(meta.len()),
            hash: None,
            hash_algorithm: None,
        }
    }

    /// Model of a directory entry, never with content.
    fn entry_model(&self, r: &Resolved, meta: &Metadata) -> ContentsModel {
        if meta.is_dir() {
            self.base_model(r, meta, ContentType::Directory)
        } else if r.api.name().ends_with(".ipynb") {
            self.base_model(r, meta, ContentType::Notebook)
        } else {
            let mut model = self.base_model(r, meta, ContentType::File);
            model.mimetype = guess_mime(&r.os);
            model
        }
    }

    fn get(&self, path: &str, options: GetOptions) -> Result<ContentsModel> {
        let r = self.resolve_for_read(path)?;
        let meta = self.stat(&r)?;
        if meta.is_dir() {
            if let Some(hint @ (ContentType::File | ContentType::Notebook)) = options.type_hint {
                return Err(Error::BadRequest(format!(
                    "{} is a directory, not a {hint}",
                    r.api
                )));
            }
            self.dir_model(&r, &meta, options.content)
        } else if meta.is_file() {
            if options.type_hint == Some(ContentType::Directory) {
                return Err(Error::BadRequest(format!("{} is not a directory", r.api)));
            }
            let as_notebook = options.type_hint == Some(ContentType::Notebook)
                || (options.type_hint.is_none() && r.api.name().ends_with(".ipynb"));
            if as_notebook {
                self.notebook_model(&r, &meta, options.content)
            } else {
                self.file_model(&r, &meta, options.content, options.format)
            }
        } else {
            Err(Error::NotFound(r.api.to_string()))
        }
    }

    fn get_without_content(&self, api: &ApiPath) -> Result<ContentsModel> {
        self.get(&api.to_string(), GetOptions::default())
    }

    fn dir_model(&self, r: &Resolved, meta: &Metadata, content: bool) -> Result<ContentsModel> {
        let mut model = self.base_model(r, meta, ContentType::Directory);
        if !content {
            return Ok(model);
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(&r.os).map_err(|e| io_to_error(e, &r.api))? {
            let entry = entry?;
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str() else {
                tracing::warn!(dir = %r.api, "skipping entry with non-UTF-8 name");
                continue;
            };
            if !self.options.allow_hidden && name.starts_with('.') {
                continue;
            }
            let child = Resolved {
                api: r.api.join(name),
                os: entry.path(),
            };
            if self.check_inside_root(&child.os).is_err() {
                tracing::debug!(path = %child.api, "skipping entry that resolves outside the root");
                continue;
            }
            let child_meta = match fs::metadata(&child.os) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(path = %child.api, error = %e, "skipping unreadable entry");
                    continue;
                }
            };
            if !(child_meta.is_dir() || child_meta.is_file()) {
                continue;
            }
            children.push(self.entry_model(&child, &child_meta));
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        let children = children
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        model.format = Some(Format::Json);
        model.content = Some(Value::Array(children));
        Ok(model)
    }

    fn file_model(
        &self,
        r: &Resolved,
        meta: &Metadata,
        content: bool,
        format: Option<Format>,
    ) -> Result<ContentsModel> {
        let mut model = self.base_model(r, meta, ContentType::File);
        let guessed = guess_mime(&r.os);
        if !content {
            model.mimetype = guessed;
            return Ok(model);
        }
        let bytes = fs::read(&r.os).map_err(|e| io_to_error(e, &r.api))?;
        let (value, format) = match format {
            None | Some(Format::Text) => match String::from_utf8(bytes) {
                Ok(text) => (text, Format::Text),
                Err(e) => {
                    if format == Some(Format::Text) {
                        return Err(Error::BadRequest(format!("{} is not UTF-8 encoded", r.api)));
                    }
                    (BASE64.encode(e.into_bytes()), Format::Base64)
                }
            },
            // jupyter_server base64-encodes for any format other than text.
            Some(Format::Base64) | Some(Format::Json) => (BASE64.encode(&bytes), Format::Base64),
        };
        let default_mime = match format {
            Format::Text => "text/plain",
            _ => "application/octet-stream",
        };
        model.mimetype = Some(guessed.unwrap_or_else(|| default_mime.to_owned()));
        model.format = Some(format);
        model.content = Some(Value::String(value));
        Ok(model)
    }

    fn notebook_model(
        &self,
        r: &Resolved,
        meta: &Metadata,
        content: bool,
    ) -> Result<ContentsModel> {
        let mut model = self.base_model(r, meta, ContentType::Notebook);
        if !content {
            return Ok(model);
        }
        let bytes = fs::read(&r.os).map_err(|e| io_to_error(e, &r.api))?;
        let notebook = Notebook::from_slice(&bytes)
            .map_err(|e| Error::BadRequest(format!("Unreadable Notebook: {} {e}", r.api)))?;
        // `nbformat.read` output, as jupyter_server serves it: multi-line
        // sources rejoined into strings, transient metadata stripped.
        model.content = Some(serde_json::to_value(&notebook)?);
        model.format = Some(Format::Json);
        Ok(model)
    }
}

// Writes.
impl Inner {
    fn save(&self, path: &str, request: SaveRequest) -> Result<ContentsModel> {
        let r = self.resolve_for_write(path)?;
        match request.kind {
            ContentType::Directory => self.save_directory(&r)?,
            ContentType::Notebook | ContentType::File => {
                let content = request
                    .content
                    .ok_or_else(|| Error::BadRequest("No file content provided".into()))?;
                if let Some(ts) = &request.last_modified {
                    self.check_conflict(&r, ts)?;
                }
                let bytes = match request.kind {
                    ContentType::Notebook => {
                        let notebook = Notebook::from_value(content)
                            .map_err(|e| Error::BadRequest(format!("Invalid notebook: {e}")))?;
                        notebook.to_file_string().into_bytes()
                    }
                    _ => file_bytes(&r.api, request.format, content)?,
                };
                if r.os.is_dir() {
                    return Err(Error::BadRequest(format!("{} is a directory", r.api)));
                }
                self.atomic_write(&r.os, &r.api, &bytes)?;
            }
        }
        self.get_without_content(&r.api)
    }

    fn save_directory(&self, r: &Resolved) -> Result<()> {
        match fs::metadata(&r.os) {
            Ok(meta) if meta.is_dir() => Ok(()),
            Ok(_) => Err(Error::BadRequest(format!("Not a directory: {}", r.api))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&r.os).map_err(|e| io_to_error(e, &r.api.parent()))
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// The external-change check. The file on disk must not be newer than
    /// the client's `last_modified` by more than [`CONFLICT_WINDOW`].
    fn check_conflict(&self, r: &Resolved, client_ts: &str) -> Result<()> {
        let client = DateTime::parse_from_rfc3339(client_ts)
            .map_err(|e| Error::BadRequest(format!("invalid last_modified {client_ts:?}: {e}")))?
            .with_timezone(&Utc);
        let meta = match fs::metadata(&r.os) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Io(e)),
        };
        let disk: DateTime<Utc> = meta.modified()?.into();
        if disk > client && disk - client > CONFLICT_WINDOW {
            return Err(Error::Conflict(format!(
                "{} changed on disk at {} after the client's copy from {}",
                r.api,
                format_datetime(disk),
                format_datetime(client)
            )));
        }
        Ok(())
    }

    /// Write `bytes` to `target` atomically: a temp file in the same
    /// directory, fsync, rename over the target. The mode of an existing
    /// target is preserved; a new file gets `0666 & !umask` like `creat(2)`.
    fn atomic_write(&self, target: &Path, api: &ApiPath, bytes: &[u8]) -> Result<()> {
        let dir = target
            .parent()
            .ok_or_else(|| Error::Forbidden("cannot write the contents root".into()))?;
        let existing_permissions = fs::metadata(target)
            .ok()
            .filter(Metadata::is_file)
            .map(|m| m.permissions());

        let prefix = format!(".~{}-", api.name());
        let mut builder = tempfile::Builder::new();
        builder.prefix(&prefix).suffix(".tmp");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut tmp = builder
            .tempfile_in(dir)
            .map_err(|e| io_to_error(e, &api.parent()))?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        if let Some(permissions) = existing_permissions {
            tmp.as_file().set_permissions(permissions)?;
        }
        tmp.persist(target).map_err(|e| Error::Io(e.error))?;
        // Make the rename durable too. Not every filesystem allows fsync on
        // a directory handle, so a failure here is not an error.
        if let Ok(dir_handle) = fs::File::open(dir) {
            let _ = dir_handle.sync_all();
        }
        Ok(())
    }

    fn delete(&self, path: &str) -> Result<()> {
        let r = self.resolve_for_write(path)?;
        if r.api.is_root() {
            return Err(Error::Forbidden("cannot delete the contents root".into()));
        }
        let meta = fs::symlink_metadata(&r.os).map_err(|e| io_to_error(e, &r.api))?;
        if meta.is_dir() {
            // A directory holding only its own checkpoint folder counts as
            // empty, as in jupyter_server.
            let checkpoint_folder = self
                .options
                .checkpoint_dir
                .is_none()
                .then_some(DEFAULT_CHECKPOINT_DIR);
            let non_empty = fs::read_dir(&r.os)?
                .filter_map(std::result::Result::ok)
                .any(|entry| checkpoint_folder.is_none_or(|cp| entry.file_name() != cp));
            if non_empty {
                return Err(Error::BadRequest(format!("Directory {} not empty", r.api)));
            }
            fs::remove_dir_all(&r.os)?;
        } else {
            fs::remove_file(&r.os)?;
        }
        self.delete_all_checkpoints(&r.api, meta.is_dir());
        Ok(())
    }

    fn rename(&self, old_path: &str, new_path: &str) -> Result<ContentsModel> {
        let from = self.resolve_for_write(old_path)?;
        let to = self.resolve_for_write(new_path)?;
        if from.api == to.api {
            return self.get_without_content(&to.api);
        }
        if from.api.is_root() || to.api.is_root() {
            return Err(Error::Forbidden("cannot rename the contents root".into()));
        }
        let meta = fs::symlink_metadata(&from.os).map_err(|e| io_to_error(e, &from.api))?;
        if fs::symlink_metadata(&to.os).is_ok() {
            return Err(Error::Conflict(format!("File already exists: {}", to.api)));
        }
        fs::rename(&from.os, &to.os).map_err(|e| io_to_error(e, &to.api.parent()))?;
        self.rename_all_checkpoints(&from.api, &to.api, meta.is_dir());
        self.get_without_content(&to.api)
    }

    fn new_untitled(
        &self,
        parent_dir: &str,
        kind: ContentType,
        ext: Option<&str>,
    ) -> Result<ContentsModel> {
        let dir = self.resolve_for_write(parent_dir)?;
        if !self.stat(&dir)?.is_dir() {
            return Err(Error::NotFound(format!("No such directory: {}", dir.api)));
        }
        let (base, ext, insert) = match kind {
            ContentType::Notebook => ("Untitled", ".ipynb".to_owned(), ""),
            ContentType::File => ("untitled", normalize_ext(ext), ""),
            ContentType::Directory => ("Untitled Folder", String::new(), " "),
        };
        let name = increment_filename(&dir.os, &format!("{base}{ext}"), insert);
        let request = match kind {
            ContentType::Notebook => SaveRequest::notebook(serde_json::to_value(Notebook::new())?),
            ContentType::File => SaveRequest::text(""),
            ContentType::Directory => SaveRequest::directory(),
        };
        self.save(&dir.api.join(&name).to_string(), request)
    }

    fn copy(&self, from_path: &str, to_dir: Option<&str>) -> Result<ContentsModel> {
        if self.options.read_only {
            return Err(Error::Forbidden("contents root is read-only".into()));
        }
        let from = self.resolve_for_read(from_path)?;
        if self.stat(&from)?.is_dir() {
            return Err(Error::BadRequest("Can't copy directories".into()));
        }
        let to_path = match to_dir {
            Some(dir) => dir.to_owned(),
            None => from.api.parent().to_string(),
        };
        let to = self.resolve_for_write(&to_path)?;
        let dest = if fs::metadata(&to.os).is_ok_and(|m| m.is_dir()) {
            let name = strip_copy_suffix(from.api.name());
            let to_name = increment_filename(&to.os, &name, "-Copy");
            Resolved {
                os: to.os.join(&to_name),
                api: to.api.join(&to_name),
            }
        } else {
            to
        };
        let bytes = fs::read(&from.os).map_err(|e| io_to_error(e, &from.api))?;
        self.atomic_write(&dest.os, &dest.api, &bytes)?;
        self.get_without_content(&dest.api)
    }
}

// Checkpoints.
impl Inner {
    /// Where the checkpoint of `api` lives. See [`FsOptions::checkpoint_dir`].
    fn checkpoint_path(&self, api: &ApiPath) -> PathBuf {
        let (stem, ext) = splitext(api.name());
        let filename = format!("{stem}-{CHECKPOINT_ID}{ext}");
        let parent = api.parent();
        match &self.options.checkpoint_dir {
            None => parent
                .os_path(&self.root)
                .join(DEFAULT_CHECKPOINT_DIR)
                .join(filename),
            Some(dir) => parent.os_path(dir).join(filename),
        }
    }

    /// The mirrored directory under an external checkpoint dir for the API
    /// directory `api`; `None` with the default per-directory layout.
    fn mirrored_checkpoint_dir(&self, api: &ApiPath) -> Option<PathBuf> {
        self.options
            .checkpoint_dir
            .as_ref()
            .map(|dir| api.os_path(dir))
    }

    fn checkpoint_file(&self, api: &ApiPath, id: &str) -> Result<PathBuf> {
        let missing = || Error::NotFound(format!("No such checkpoint: {id} for {api}"));
        if id != CHECKPOINT_ID {
            return Err(missing());
        }
        let cp = self.checkpoint_path(api);
        if !cp.is_file() {
            return Err(missing());
        }
        Ok(cp)
    }

    fn checkpoint_model(&self, cp: &Path) -> Result<Checkpoint> {
        let meta = fs::metadata(cp)?;
        Ok(Checkpoint {
            id: CHECKPOINT_ID.to_owned(),
            last_modified: format_time(meta.modified()?),
        })
    }

    fn list_checkpoints(&self, path: &str) -> Result<Vec<Checkpoint>> {
        let r = self.resolve_for_read(path)?;
        let cp = self.checkpoint_path(&r.api);
        if cp.is_file() {
            Ok(vec![self.checkpoint_model(&cp)?])
        } else {
            Ok(Vec::new())
        }
    }

    fn create_checkpoint(&self, path: &str) -> Result<Checkpoint> {
        let r = self.resolve_for_write(path)?;
        if self.stat(&r)?.is_dir() {
            return Err(Error::BadRequest(format!(
                "Checkpoints are not supported for directories: {}",
                r.api
            )));
        }
        let cp = self.checkpoint_path(&r.api);
        if let Some(parent) = cp.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = fs::read(&r.os).map_err(|e| io_to_error(e, &r.api))?;
        self.atomic_write(&cp, &r.api, &bytes)?;
        self.checkpoint_model(&cp)
    }

    fn restore_checkpoint(&self, path: &str, id: &str) -> Result<()> {
        let r = self.resolve_for_write(path)?;
        let cp = self.checkpoint_file(&r.api, id)?;
        let bytes = fs::read(&cp)?;
        self.atomic_write(&r.os, &r.api, &bytes)
    }

    fn delete_checkpoint(&self, path: &str, id: &str) -> Result<()> {
        let r = self.resolve_for_write(path)?;
        let cp = self.checkpoint_file(&r.api, id)?;
        fs::remove_file(cp)?;
        Ok(())
    }

    /// Best effort: the file or directory itself is already gone.
    fn delete_all_checkpoints(&self, api: &ApiPath, was_dir: bool) {
        let cp = self.checkpoint_path(api);
        if cp.is_file() {
            if let Err(e) = fs::remove_file(&cp) {
                tracing::warn!(path = %api, error = %e, "could not delete checkpoint");
            }
        }
        if was_dir {
            if let Some(mirror) = self.mirrored_checkpoint_dir(api) {
                if mirror.is_dir() {
                    if let Err(e) = fs::remove_dir_all(&mirror) {
                        tracing::warn!(path = %api, error = %e, "could not delete checkpoints");
                    }
                }
            }
        }
    }

    /// Best effort: the rename itself already happened.
    fn rename_all_checkpoints(&self, from: &ApiPath, to: &ApiPath, is_dir: bool) {
        let mut moves = Vec::new();
        let (cp_from, cp_to) = (self.checkpoint_path(from), self.checkpoint_path(to));
        if cp_from.is_file() {
            moves.push((cp_from, cp_to));
        }
        if is_dir {
            if let (Some(a), Some(b)) = (
                self.mirrored_checkpoint_dir(from),
                self.mirrored_checkpoint_dir(to),
            ) {
                if a.is_dir() {
                    moves.push((a, b));
                }
            }
        }
        for (a, b) in moves {
            let result = b
                .parent()
                .map(fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|()| fs::rename(&a, &b));
            if let Err(e) = result {
                tracing::warn!(from = %from, to = %to, error = %e, "could not move checkpoint");
            }
        }
    }
}

/// Map an I/O error on `api` to the explicit variant when it has one.
fn io_to_error(e: io::Error, api: &ApiPath) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound(api.to_string()),
        io::ErrorKind::PermissionDenied => Error::Forbidden(format!("permission denied: {api}")),
        _ => Error::Io(e),
    }
}

/// Bytes to write for a file save, per `jupyter_server`'s `_save_file`.
fn file_bytes(api: &ApiPath, format: Option<Format>, content: Value) -> Result<Vec<u8>> {
    let format = match format {
        Some(f @ (Format::Text | Format::Base64)) => f,
        _ => {
            return Err(Error::BadRequest(
                "Must specify format of file contents as 'text' or 'base64'".into(),
            ))
        }
    };
    let Value::String(text) = content else {
        return Err(Error::BadRequest(format!(
            "Encoding error saving {api}: file content must be a string"
        )));
    };
    match format {
        Format::Text => Ok(text.into_bytes()),
        _ => {
            let cleaned: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            BASE64_LENIENT
                .decode(cleaned)
                .map_err(|e| Error::BadRequest(format!("Encoding error saving {api}: {e}")))
        }
    }
}

fn guess_mime(path: &Path) -> Option<String> {
    mime_guess::from_path(path).first_raw().map(str::to_owned)
}

/// RFC 3339 UTC with microseconds, `jupyter_server`'s timestamp form.
fn format_time(t: SystemTime) -> String {
    format_datetime(t.into())
}

fn format_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

/// Birth time where the platform reports one, else the inode change time on
/// Unix, else the modification time.
fn created_time(meta: &Metadata) -> SystemTime {
    if let Ok(t) = meta.created() {
        return t;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(secs), Ok(nanos)) = (
            u64::try_from(meta.ctime()),
            u32::try_from(meta.ctime_nsec()),
        ) {
            return SystemTime::UNIX_EPOCH + Duration::new(secs, nanos);
        }
    }
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `.txt` for `txt` or `.txt`; `""` for `None`.
fn normalize_ext(ext: Option<&str>) -> String {
    match ext {
        None | Some("") => String::new(),
        Some(e) if e.starts_with('.') => e.to_owned(),
        Some(e) => format!(".{e}"),
    }
}

/// `jupyter_server`'s `increment_filename`: the first of `name`,
/// `stem<insert>1<suffix>`, `stem<insert>2<suffix>`, ... that does not exist
/// in `dir`. The suffix is `.ipynb` for notebooks and everything from the
/// first dot otherwise (`a.tar.gz` -> `a`, `.tar.gz`).
fn increment_filename(dir: &Path, filename: &str, insert: &str) -> String {
    let (stem, suffix) = match filename.strip_suffix(".ipynb") {
        Some(stem) => (stem, ".ipynb"),
        None => filename.split_at(filename.find('.').unwrap_or(filename.len())),
    };
    let mut i = 0u32;
    loop {
        let name = if i == 0 {
            format!("{stem}{suffix}")
        } else {
            format!("{stem}{insert}{i}{suffix}")
        };
        if fs::symlink_metadata(dir.join(&name)).is_err() {
            return name;
        }
        i += 1;
    }
}

/// `jupyter_server`'s `copy_pat = re.compile(r"\-Copy\d*\.")`, substituted
/// with `.`: `x-Copy1.ipynb` -> `x.ipynb`, `x-Copy.txt` -> `x.txt`.
fn strip_copy_suffix(name: &str) -> String {
    const MARKER: &str = "-Copy";
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(i) = rest.find(MARKER) {
        let after = &rest[i + MARKER.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if after[digits..].starts_with('.') {
            out.push_str(&rest[..i]);
            rest = &after[digits..];
        } else {
            out.push_str(&rest[..i + MARKER.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Python's `os.path.splitext`: the extension starts at the last dot, unless
/// every character before that dot is also a dot (`.bashrc` has none).
fn splitext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if !name[..i].trim_start_matches('.').is_empty() => name.split_at(i),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_suffix_stripping() {
        assert_eq!(strip_copy_suffix("x-Copy1.ipynb"), "x.ipynb");
        assert_eq!(strip_copy_suffix("x-Copy12.txt"), "x.txt");
        assert_eq!(strip_copy_suffix("x-Copy.txt"), "x.txt");
        assert_eq!(strip_copy_suffix("x.txt"), "x.txt");
        assert_eq!(strip_copy_suffix("-Copyright.txt"), "-Copyright.txt");
        assert_eq!(strip_copy_suffix("a-Copy1-Copy2.md"), "a-Copy1.md");
    }

    #[test]
    fn splitext_like_python() {
        assert_eq!(splitext("a.ipynb"), ("a", ".ipynb"));
        assert_eq!(splitext("a.tar.gz"), ("a.tar", ".gz"));
        assert_eq!(splitext(".bashrc"), (".bashrc", ""));
        assert_eq!(splitext("..foo"), ("..foo", ""));
        assert_eq!(splitext(".a.b"), (".a", ".b"));
        assert_eq!(splitext("noext"), ("noext", ""));
    }

    #[test]
    fn increment_in_empty_dir_is_identity() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            increment_filename(dir.path(), "Untitled.ipynb", ""),
            "Untitled.ipynb"
        );
        fs::write(dir.path().join("a.tar.gz"), b"").unwrap();
        assert_eq!(
            increment_filename(dir.path(), "a.tar.gz", "-Copy"),
            "a-Copy1.tar.gz"
        );
    }

    #[test]
    fn timestamp_format() {
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_792_483_200, 123_456_789);
        assert_eq!(format_time(t), "2026-10-20T08:00:00.123456Z");
        assert_eq!(
            format_time(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00.000000Z"
        );
    }

    #[test]
    fn ext_normalization() {
        assert_eq!(normalize_ext(None), "");
        assert_eq!(normalize_ext(Some("")), "");
        assert_eq!(normalize_ext(Some(".txt")), ".txt");
        assert_eq!(normalize_ext(Some("txt")), ".txt");
    }
}
