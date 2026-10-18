//! Notebook trust: nbformat's `NotebookNotary`, so HTML and JavaScript
//! outputs of a reopened notebook render instead of being sanitized.
//!
//! A notebook is trusted when an HMAC-SHA256 of its content, keyed with a
//! per-user secret, is in a store of signatures this user produced. The
//! digest is computed as `nbformat.sign.NotebookNotary.compute_signature`
//! does, over the notebook in memory as `nbformat.read` returns it (see
//! [`Notary::compute_signature`]), and the secret and store are Jupyter's
//! own: `notebook_secret` and `nbsignatures.db` in the Jupyter data
//! directory. A notebook trusted by Jupyter is trusted here and the other way
//! round.
//!
//! The rules are `jupyter_server`'s:
//!
//! - On read, every code cell gets `metadata.trusted`, `true` when the
//!   notebook's signature is in the store ([`mark_cells`]).
//! - On save, the notebook is signed when every code cell is trusted: the
//!   client marked it `trusted` (JupyterLab does for cells whose output was
//!   produced in this session) or it has no output other than streams and
//!   errors ([`check_cells`]).
//! - `POST /api/contents/{path}/trust` and `ganymede trust` sign
//!   unconditionally.
//!
//! The SQLite store needs the `sqlite` feature (SQLite compiled from C by
//! `rusqlite`). Without it, [`Notary::open`] keeps signatures in memory for
//! the life of the process, as nbformat does when `sqlite3` is missing.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::{Error, Result};

/// Name of the secret key file in the data directory.
pub const SECRET_FILE: &str = "notebook_secret";
/// Name of the SQLite signature database in the data directory.
pub const DB_FILE: &str = "nbsignatures.db";
/// The hash algorithm, as recorded in the store. nbformat's default and the
/// only one Ganymede computes.
pub const ALGORITHM: &str = "sha256";
/// Signatures kept before the oldest quarter is dropped
/// (`SQLiteSignatureStore.cache_size`).
pub const CACHE_SIZE: usize = 65535;

/// Where signatures of trusted notebooks are kept
/// (`nbformat.sign.SignatureStore`).
pub trait SignatureStore: Send + Sync + fmt::Debug {
    /// Record a signature. Recording one already present refreshes it.
    fn store_signature(&self, digest: &str, algorithm: &str) -> Result<()>;
    /// Whether the signature is recorded.
    fn check_signature(&self, digest: &str, algorithm: &str) -> Result<bool>;
    /// Forget a signature; not an error when it is absent.
    fn remove_signature(&self, digest: &str, algorithm: &str) -> Result<()>;
}

/// Signatures in memory, for this process only
/// (`nbformat.sign.MemorySignatureStore`).
#[derive(Debug, Default)]
pub struct MemorySignatureStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    /// Signature to the tick it was last stored or checked.
    seen: HashMap<(String, String), u64>,
    tick: u64,
}

impl MemorySignatureStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SignatureStore for MemorySignatureStore {
    fn store_signature(&self, digest: &str, algorithm: &str) -> Result<()> {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.tick += 1;
        let tick = m.tick;
        m.seen
            .insert((digest.to_owned(), algorithm.to_owned()), tick);
        if m.seen.len() >= CACHE_SIZE {
            // Drop the oldest 25%, as MemorySignatureStore._maybe_cull does.
            let mut ticks: Vec<u64> = m.seen.values().copied().collect();
            ticks.sort_unstable();
            let cutoff = ticks[m.seen.len() / 4];
            m.seen.retain(|_, t| *t >= cutoff);
        }
        Ok(())
    }

    fn check_signature(&self, digest: &str, algorithm: &str) -> Result<bool> {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.tick += 1;
        let tick = m.tick;
        match m.seen.get_mut(&(digest.to_owned(), algorithm.to_owned())) {
            Some(t) => {
                *t = tick;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn remove_signature(&self, digest: &str, algorithm: &str) -> Result<()> {
        let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        m.seen.remove(&(digest.to_owned(), algorithm.to_owned()));
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteSignatureStore;

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use rusqlite::{params, Connection, OptionalExtension};
    use std::time::Duration;

    /// Jupyter's `nbsignatures.db`, read and written as
    /// `nbformat.sign.SQLiteSignatureStore` does: same table, same index,
    /// same `last_seen` timestamps, same culling.
    #[derive(Debug)]
    pub struct SqliteSignatureStore {
        db: Mutex<Connection>,
        path: PathBuf,
    }

    fn db_error(e: rusqlite::Error) -> Error {
        Error::Io(io::Error::other(e))
    }

    impl SqliteSignatureStore {
        /// Open or create the database at `path`.
        ///
        /// A file that is not a usable database is renamed to `<path>.bak`
        /// and a new one created; if that fails too, signatures are kept in
        /// an in-memory database for this process. Both are what nbformat
        /// does, with the same warnings.
        pub fn open(path: impl Into<PathBuf>) -> Self {
            let path = path.into();
            let db = match Self::connect(&path) {
                Ok(db) => db,
                Err(e) => {
                    let mut bak = path.clone().into_os_string();
                    bak.push(".bak");
                    tracing::warn!(
                        "The signatures database {} cannot be opened ({e}); maybe it is corrupted \
                         or encrypted. You may need to rerun your notebooks to ensure that they \
                         are trusted to run Javascript. The old signatures database has been \
                         renamed to {} and a new one has been created.",
                        path.display(),
                        PathBuf::from(&bak).display()
                    );
                    match fs::rename(&path, &bak)
                        .map_err(Error::Io)
                        .and_then(|()| Self::connect(&path))
                    {
                        Ok(db) => db,
                        Err(e) => {
                            tracing::warn!(
                                "Failed committing signatures database to disk ({e}). Using an \
                                 in-memory signatures database for the remainder of this session."
                            );
                            Self::connect_memory()
                        }
                    }
                }
            };
            Self {
                db: Mutex::new(db),
                path,
            }
        }

        /// The database file.
        pub fn path(&self) -> &Path {
            &self.path
        }

        fn connect(path: &Path) -> Result<Connection> {
            let db = Connection::open(path).map_err(db_error)?;
            Self::init(&db)?;
            Ok(db)
        }

        fn connect_memory() -> Connection {
            let db = Connection::open_in_memory().expect("in-memory SQLite always opens");
            Self::init(&db).expect("in-memory SQLite always initializes");
            db
        }

        fn init(db: &Connection) -> Result<()> {
            // jupyter_server and other processes share the file.
            db.busy_timeout(Duration::from_secs(5)).map_err(db_error)?;
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS nbsignatures
                (
                    id integer PRIMARY KEY AUTOINCREMENT,
                    algorithm text,
                    signature text,
                    path text,
                    last_seen timestamp
                );
                CREATE INDEX IF NOT EXISTS algosig ON nbsignatures(algorithm, signature);",
            )
            .map_err(db_error)
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
            self.db.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn touch(db: &Connection, digest: &str, algorithm: &str) -> Result<()> {
            db.execute(
                "UPDATE nbsignatures SET last_seen = ?1 WHERE algorithm = ?2 AND signature = ?3",
                params![now_isoformat(), algorithm, digest],
            )
            .map_err(db_error)?;
            Ok(())
        }

        fn exists(db: &Connection, digest: &str, algorithm: &str) -> Result<bool> {
            Ok(db
                .query_row(
                    "SELECT id FROM nbsignatures WHERE algorithm = ?1 AND signature = ?2",
                    params![algorithm, digest],
                    |_| Ok(()),
                )
                .optional()
                .map_err(db_error)?
                .is_some())
        }
    }

    impl SignatureStore for SqliteSignatureStore {
        fn store_signature(&self, digest: &str, algorithm: &str) -> Result<()> {
            let db = self.lock();
            if Self::exists(&db, digest, algorithm)? {
                Self::touch(&db, digest, algorithm)?;
            } else {
                db.execute(
                    "INSERT INTO nbsignatures (algorithm, signature, last_seen) VALUES (?1, ?2, ?3)",
                    params![algorithm, digest, now_isoformat()],
                )
                .map_err(db_error)?;
            }
            let n: i64 = db
                .query_row("SELECT Count(*) FROM nbsignatures", [], |r| r.get(0))
                .map_err(db_error)?;
            if n > CACHE_SIZE as i64 {
                // Keep the newest 75%, as SQLiteSignatureStore.cull_db does.
                let keep = ((0.75 * CACHE_SIZE as f64) as i64).max(1);
                db.execute(
                    "DELETE FROM nbsignatures WHERE id IN (
                        SELECT id FROM nbsignatures ORDER BY last_seen DESC LIMIT -1 OFFSET ?1
                    )",
                    params![keep],
                )
                .map_err(db_error)?;
            }
            Ok(())
        }

        fn check_signature(&self, digest: &str, algorithm: &str) -> Result<bool> {
            let db = self.lock();
            if !Self::exists(&db, digest, algorithm)? {
                return Ok(false);
            }
            Self::touch(&db, digest, algorithm)?;
            Ok(true)
        }

        fn remove_signature(&self, digest: &str, algorithm: &str) -> Result<()> {
            self.lock()
                .execute(
                    "DELETE FROM nbsignatures WHERE algorithm = ?1 AND signature = ?2",
                    params![algorithm, digest],
                )
                .map_err(db_error)?;
            Ok(())
        }
    }

    /// `datetime.now(tz=timezone.utc).isoformat()`, the form nbformat
    /// stores: microseconds only when non-zero, `+00:00`.
    fn now_isoformat() -> String {
        let now = chrono::Utc::now();
        let micros = now.timestamp_subsec_micros();
        if micros == 0 {
            now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
        } else {
            format!("{}.{micros:06}+00:00", now.format("%Y-%m-%dT%H:%M:%S"))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn isoformat_shape() {
            let s = now_isoformat();
            assert!(s.ends_with("+00:00"), "{s}");
            assert!(chrono::DateTime::parse_from_rfc3339(&s).is_ok(), "{s}");
        }
    }
}

/// Computes, checks and records notebook signatures
/// (`nbformat.sign.NotebookNotary`).
pub struct Notary {
    secret: Vec<u8>,
    store: Box<dyn SignatureStore>,
    secret_file: Option<PathBuf>,
    db_file: Option<PathBuf>,
}

impl fmt::Debug for Notary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notary")
            .field("secret", &"<redacted>")
            .field("store", &self.store)
            .field("secret_file", &self.secret_file)
            .field("db_file", &self.db_file)
            .finish()
    }
}

impl Notary {
    /// A notary with the given key and store, touching no files.
    pub fn new(secret: impl Into<Vec<u8>>, store: Box<dyn SignatureStore>) -> Self {
        Self {
            secret: secret.into(),
            store,
            secret_file: None,
            db_file: None,
        }
    }

    /// A notary with a fresh random key and an in-memory store: nothing
    /// signed by it outlives the process. For when there is no data
    /// directory to keep a key in.
    pub fn ephemeral() -> Result<Self> {
        Ok(Self::new(
            generate_secret()?,
            Box::new(MemorySignatureStore::new()),
        ))
    }

    /// Jupyter's notary for `data_dir` (the Jupyter data directory, e.g.
    /// `~/.local/share/jupyter`): the key in `notebook_secret`, created with
    /// mode 0600 if missing, and the signatures in `nbsignatures.db` (in
    /// memory without the `sqlite` feature).
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir)?;
        let secret_file = data_dir.join(SECRET_FILE);
        let secret = read_or_create_secret(&secret_file)?;
        let db_file = data_dir.join(DB_FILE);
        #[cfg(feature = "sqlite")]
        let store: Box<dyn SignatureStore> = Box::new(SqliteSignatureStore::open(&db_file));
        #[cfg(not(feature = "sqlite"))]
        let store: Box<dyn SignatureStore> = Box::new(MemorySignatureStore::new());
        Ok(Self {
            secret,
            store,
            secret_file: Some(secret_file),
            db_file: cfg!(feature = "sqlite").then_some(db_file),
        })
    }

    /// The key file, when the notary was opened on a data directory.
    pub fn secret_file(&self) -> Option<&Path> {
        self.secret_file.as_deref()
    }

    /// The signature database, when it is a file.
    pub fn db_file(&self) -> Option<&Path> {
        self.db_file.as_deref()
    }

    /// HMAC-SHA256 of the notebook, hex encoded, exactly as
    /// `NotebookNotary.compute_signature` computes it over the same
    /// notebook dict: every key of every object in sorted order followed by
    /// its value, strings as UTF-8, other scalars as `str()` writes them in
    /// nbformat (`True`, `None`, `1.5`, `1e-05`), skipping
    /// `metadata.signature`.
    ///
    /// `nb` should be the notebook as it is served (`nbformat.read`'s
    /// result, multi-line strings joined), without cell `trusted` flags;
    /// those are hashed if present, as in nbformat.
    pub fn compute_signature(&self, nb: &Value) -> String {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("hmac accepts any key length");
        let mut feed = |b: &[u8]| mac.update(b);
        match nb {
            Value::Object(top) => {
                for (key, value) in sorted(top) {
                    feed(key.as_bytes());
                    if key == "metadata" {
                        if let Value::Object(metadata) = value {
                            for (k, v) in sorted(metadata) {
                                if k != "signature" {
                                    feed(k.as_bytes());
                                    yield_everything(v, &mut feed);
                                }
                            }
                            continue;
                        }
                    }
                    yield_everything(value, &mut feed);
                }
            }
            other => yield_everything(other, &mut feed),
        }
        hex::encode(mac.finalize().into_bytes())
    }

    /// Whether the notebook's signature is in the store. A store error is
    /// logged and counts as untrusted.
    pub fn check_signature(&self, nb: &Value) -> bool {
        let digest = self.compute_signature(nb);
        self.store
            .check_signature(&digest, ALGORITHM)
            .unwrap_or_else(|e| {
                tracing::warn!("could not read the notebook signature store: {e}");
                false
            })
    }

    /// Record the notebook's signature: its output is trusted from now on.
    pub fn sign(&self, nb: &Value) -> Result<()> {
        let digest = self.compute_signature(nb);
        self.store.store_signature(&digest, ALGORITHM)
    }

    /// Forget the notebook's signature.
    pub fn unsign(&self, nb: &Value) -> Result<()> {
        let digest = self.compute_signature(nb);
        self.store.remove_signature(&digest, ALGORITHM)
    }
}

fn sorted(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = map.iter().collect();
    // Byte order of UTF-8 is code point order, which is how nbformat's
    // `sorted()` orders keys.
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

/// nbformat's `yield_everything`.
fn yield_everything(value: &Value, feed: &mut impl FnMut(&[u8])) {
    match value {
        Value::Object(map) => {
            for (k, v) in sorted(map) {
                feed(k.as_bytes());
                yield_everything(v, feed);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| yield_everything(v, feed)),
        Value::String(s) => feed(s.as_bytes()),
        Value::Null => feed(b"None"),
        Value::Bool(true) => feed(b"True"),
        Value::Bool(false) => feed(b"False"),
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                feed(ganymede_nbformat::python_float_repr(f).as_bytes())
            } else {
                feed(n.to_string().as_bytes())
            }
        }
    }
}

/// Set `metadata.trusted` on every code cell (`NotebookNotary.mark_cells`).
pub fn mark_cells(nb: &mut Value, trusted: bool) {
    for cell in code_cells(nb) {
        if let Some(Value::Object(metadata)) = cell.get_mut("metadata") {
            metadata.insert("trusted".into(), Value::Bool(trusted));
        }
    }
}

/// Whether every code cell is trusted (`NotebookNotary.check_cells`): its
/// `metadata.trusted` is truthy, or none of its `execute_result` and
/// `display_data` outputs has anything besides `output_type`,
/// `execution_count` and `metadata`. Removes `trusted` from code cells, as
/// nbformat does, so the result can be signed.
pub fn check_cells(nb: &mut Value) -> bool {
    let mut all = true;
    for cell in code_cells(nb) {
        let flagged = match cell.get_mut("metadata") {
            Some(Value::Object(metadata)) => metadata.remove("trusted").is_some_and(|v| truthy(&v)),
            _ => false,
        };
        if flagged {
            continue;
        }
        let unsafe_output = cell
            .get("outputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .any(|output| {
                matches!(
                    output.get("output_type").and_then(Value::as_str),
                    Some("execute_result" | "display_data")
                ) && output
                    .keys()
                    .any(|k| !matches!(k.as_str(), "output_type" | "execution_count" | "metadata"))
            });
        if unsafe_output {
            all = false;
        }
    }
    all
}

fn code_cells(nb: &mut Value) -> impl Iterator<Item = &mut Map<String, Value>> {
    nb.get_mut("cells")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
        .filter(|cell| cell.get("cell_type").and_then(Value::as_str) == Some("code"))
}

/// Truth value of a JSON value as nbformat tests it (`bool(value)`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A new key as nbformat generates one: `base64.encodebytes` of 1024
/// random bytes (76-character lines, each ending in a newline).
pub fn generate_secret() -> Result<Vec<u8>> {
    use base64::Engine;
    let mut raw = [0u8; 1024];
    getrandom::fill(&mut raw).map_err(|e| Error::Io(io::Error::other(e)))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    let mut out = Vec::with_capacity(encoded.len() + encoded.len() / 76 + 1);
    for line in encoded.as_bytes().chunks(76) {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    Ok(out)
}

/// Read the key file, or create it with a new key, readable only by its
/// owner. The key is the file's bytes, newlines included, as nbformat uses
/// it. An empty key file (left by a crash in an older version) signs
/// nothing meaningfully, so it is replaced with a new key.
fn read_or_create_secret(path: &Path) -> Result<Vec<u8>> {
    match fs::read(path) {
        Ok(secret) if !secret.is_empty() => return Ok(secret),
        Ok(_) => {
            tracing::warn!(
                "Notebook-signing key {} is empty; replacing it",
                path.display()
            );
            let secret = generate_secret()?;
            write_secret(path, &secret)?;
            return Ok(secret);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io(e)),
    }
    let secret = generate_secret()?;
    // Write the whole key to a temp file, then link it into place without
    // replacing, so the key file is never seen half-written.
    match private_temp(path, &secret)?.persist_noclobber(path) {
        Ok(_) => {
            tracing::info!("Writing notebook-signing key to {}", path.display());
            sync_parent(path);
            Ok(secret)
        }
        // Another process created it first; use theirs.
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
            let theirs = fs::read(path)?;
            if theirs.is_empty() {
                return Err(Error::Io(io::Error::other(format!(
                    "notebook-signing key {} is empty",
                    path.display()
                ))));
            }
            Ok(theirs)
        }
        Err(e) => Err(Error::Io(e.error)),
    }
}

/// `secret` in a mode-0600 temp file next to `path`, synced to disk.
fn private_temp(path: &Path, secret: &[u8]) -> Result<tempfile::NamedTempFile> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".notebook_secret-").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o600));
    }
    let mut tmp = builder.tempfile_in(dir)?;
    tmp.write_all(secret)?;
    tmp.as_file().sync_all()?;
    Ok(tmp)
}

fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Ok(handle) = fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
}

/// Replace the key with `secret`, as `jupyter trust --reset` does, mode
/// 0600. Every existing signature stops matching. The new key goes to a
/// temp file that is renamed over the old one, so a crash leaves one key
/// or the other, never an empty file.
pub fn write_secret(path: &Path, secret: &[u8]) -> Result<()> {
    if secret.is_empty() {
        return Err(Error::BadRequest("notebook-signing key is empty".into()));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    private_temp(path, secret)?
        .persist(path)
        .map_err(|e| Error::Io(e.error))?;
    sync_parent(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notary() -> Notary {
        Notary::new(b"k".to_vec(), Box::new(MemorySignatureStore::new()))
    }

    #[test]
    fn scalars_hash_as_python_str() {
        let mut bytes = Vec::new();
        yield_everything(
            &json!({"b": [true, false, null], "a": 1, "c": 1.0, "d": 1e-5, "é": "x", "Z": -2}),
            &mut |b: &[u8]| bytes.extend_from_slice(b),
        );
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "Z-2a1bTrueFalseNonec1.0d1e-05éx"
        );
    }

    #[test]
    fn signature_skips_only_top_level_signature() {
        let n = notary();
        let plain = json!({"metadata": {"a": 1}, "cells": []});
        let signed = json!({"metadata": {"a": 1, "signature": "sha256:x"}, "cells": []});
        let nested = json!({"metadata": {"a": 1}, "cells": [{"metadata": {"signature": 1}}]});
        assert_eq!(n.compute_signature(&plain), n.compute_signature(&signed));
        assert_ne!(n.compute_signature(&plain), n.compute_signature(&nested));
    }

    #[test]
    fn sign_check_unsign() {
        let n = notary();
        let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": []});
        assert!(!n.check_signature(&nb));
        n.sign(&nb).unwrap();
        assert!(n.check_signature(&nb));
        let other = Notary::new(b"other".to_vec(), Box::new(MemorySignatureStore::new()));
        assert!(!other.check_signature(&nb));
        n.unsign(&nb).unwrap();
        assert!(!n.check_signature(&nb));
    }

    #[test]
    fn memory_store_culls_the_oldest_quarter() {
        let store = MemorySignatureStore::new();
        for i in 0..CACHE_SIZE {
            store.store_signature(&i.to_string(), ALGORITHM).unwrap();
        }
        let left = store.inner.lock().unwrap().seen.len();
        assert_eq!(left, CACHE_SIZE - CACHE_SIZE / 4);
        assert!(!store.check_signature("0", ALGORITHM).unwrap());
        assert!(store
            .check_signature(&(CACHE_SIZE - 1).to_string(), ALGORITHM)
            .unwrap());
    }

    #[test]
    fn generated_secret_has_nbformat_shape() {
        let s = generate_secret().unwrap();
        let text = String::from_utf8(s).unwrap();
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(lines.len(), 18);
        assert!(lines.iter().all(|l| l.len() == 76));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn empty_secret_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SECRET_FILE);
        fs::write(&file, b"").unwrap();
        let a = Notary::open(dir.path()).unwrap();
        let key = fs::read(&file).unwrap();
        assert!(key.len() > 1000, "a new key was written");
        let nb = json!({"cells": []});
        let b = Notary::open(dir.path()).unwrap();
        assert_eq!(a.compute_signature(&nb), b.compute_signature(&nb));
        assert!(write_secret(&file, b"").is_err());
        assert_eq!(fs::read(&file).unwrap(), key);
    }

    #[test]
    fn write_secret_replaces_privately_without_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SECRET_FILE);
        fs::write(&file, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_secret(&file, b"new").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn secret_file_is_created_private_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let a = Notary::open(dir.path()).unwrap();
        let file = dir.path().join(SECRET_FILE);
        assert_eq!(a.secret_file(), Some(file.as_path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let b = Notary::open(dir.path()).unwrap();
        let nb = json!({"cells": []});
        assert_eq!(a.compute_signature(&nb), b.compute_signature(&nb));
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn signatures_persist_in_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let nb = json!({"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": []});
        Notary::open(dir.path()).unwrap().sign(&nb).unwrap();
        let again = Notary::open(dir.path()).unwrap();
        assert_eq!(again.db_file(), Some(dir.path().join(DB_FILE).as_path()));
        assert!(again.check_signature(&nb));
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn corrupt_database_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(DB_FILE);
        fs::write(
            &db,
            b"not a database, but long enough to have a header page....",
        )
        .unwrap();
        let store = SqliteSignatureStore::open(&db);
        store.store_signature("abc", ALGORITHM).unwrap();
        assert!(store.check_signature("abc", ALGORITHM).unwrap());
        assert!(dir.path().join("nbsignatures.db.bak").is_file());
    }

    #[test]
    fn truthiness_like_python() {
        for v in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&v), "{v}");
        }
        for v in [
            json!(true),
            json!(1),
            json!(-0.5),
            json!("x"),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(&v), "{v}");
        }
    }
}
