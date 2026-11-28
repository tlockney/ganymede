//! `ganymede.toml` as the one file the Mac app and `ganymede serve` share
//! (#995): read it as JSON, and set keys in it without losing comments,
//! order or other keys.
//!
//! [`read`] gives the file's keys as a JSON object, as the app passes them
//! to [`ganymede_server_start`](crate::serve)'s `config_json`. [`set`]
//! applies a JSON object to the file with `toml_edit`, so an edit keeps
//! everything it doesn't name, and an edit that would make the file
//! invalid, as [`Config`] reads it, is refused and nothing is written.
//! Swift never writes TOML.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value as JsonValue};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::config::Config;
use crate::serve::{default_config, ganymede_config_dir, DEFAULT_CONFIG_FILE};

/// The config file [`read`] and [`set`] use: [`DEFAULT_CONFIG_FILE`] in
/// `dir`, or, with `dir` None, in [`ganymede_config_dir`].
pub fn config_file_path(dir: Option<&Path>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(dir.join(DEFAULT_CONFIG_FILE)),
        None => ganymede_config_dir()
            .map(|dir| dir.join(DEFAULT_CONFIG_FILE))
            .ok_or_else(|| anyhow!("no config dir: set GANYMEDE_CONFIG_DIR")),
    }
}

/// The file's keys as a JSON object; an empty object when there is no file.
///
/// With `dir` None and no file in Ganymede's config dir, the file
/// [`default_config`] finds, which may be the legacy one in the Jupyter
/// config dir that `ganymede serve` reads. The keys are not checked
/// against [`Config`]: this returns what the file holds, so an older
/// `ganymede serve`'s keys or a newer one's survive the round trip.
/// Strings, integers, floats, booleans and arrays are themselves, tables
/// are objects, and a datetime is its TOML string. The object's keys are
/// sorted, not in the file's order: [`set`] is what keeps that.
pub fn read(dir: Option<&Path>) -> Result<Map<String, JsonValue>> {
    let path = config_file_path(dir)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => match legacy_path(&path, dir) {
            Some(legacy) => std::fs::read_to_string(&legacy)
                .with_context(|| format!("reading {}", legacy.display()))?,
            None => return Ok(Map::new()),
        },
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let table = toml::from_str::<toml::Table>(&text)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(table.into_iter().map(|(k, v)| (k, to_json(&v))).collect())
}

/// Applies `patch` to the file and writes it back.
///
/// A key in `patch` sets the file's key; `null` removes it; an object
/// merges into that table, recursively. Comments, order and keys the patch
/// doesn't name are kept, and an edit that would make the file invalid, as
/// [`Config`] reads it, is refused with nothing written.
///
/// With `dir` None and no file in Ganymede's config dir but the legacy one
/// in the Jupyter config dir, this is an error: a new file here would make
/// the server read no file at all, and ignore the settings it has.
///
/// A symlinked file is written where the link points, so a dotfile
/// manager's link stays one. The file is replaced atomically, with mode
/// 0600, and its dir is created 0700 when missing. Nothing is locked: each
/// call re-reads the file, so two writers lose only a concurrent edit made
/// during the call's own read and write, which is a moment.
pub fn set(dir: Option<&Path>, patch: &JsonValue) -> Result<()> {
    let path = config_file_path(dir)?;
    if !exists(&path) {
        refuse_legacy(&path, legacy_path(&path, dir).as_deref())?;
    }
    write(&path, patch)
}

/// `text` with `patch` applied, validated as [`Config`].
///
/// The pure half of [`set`]: a table created for an object is a standard
/// table when it goes in a standard table or the document, and an inline
/// table inside an inline table. Numbers that fit an `i64` are TOML
/// integers, others floats, objects in arrays inline tables, and `null` in
/// an array an error.
pub fn apply(text: &str, patch: &JsonValue) -> Result<String> {
    apply_to(text, patch, "the config")
}

/// The file `ganymede serve` would read in this call's place: the legacy
/// one in the Jupyter config dir, when `dir` is None, the file is not
/// there, and [`default_config`] finds one.
fn legacy_path(path: &Path, dir: Option<&Path>) -> Option<PathBuf> {
    if dir.is_some() || exists(path) {
        return None;
    }
    default_config().path.filter(|p| p != path)
}

/// Refuses a write the server would not read: `serving` is the file
/// `ganymede serve` reads now, which a new file at `path` would make it
/// ignore silently.
fn refuse_legacy(path: &Path, serving: Option<&Path>) -> Result<()> {
    match serving.filter(|p| *p != path) {
        Some(old) => bail!(
            "{} is in the Jupyter config dir; move it to {} before changing settings",
            old.display(),
            path.display()
        ),
        None => Ok(()),
    }
}

/// Whether anything is at `path`, a dangling symlink included.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Reads, edits and writes the file at `path`.
fn write(path: &Path, patch: &JsonValue) -> Result<()> {
    let path = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .with_context(|| format!("resolving the link at {}", path.display()))?,
        _ => path.to_path_buf(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let edited = apply_to(&text, patch, &path.display().to_string())?;
    if edited == text {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        create_dir(dir)?;
    }
    write_atomic(&path, &edited)
}

/// Applies `patch` to `text` and checks the result as a [`Config`]; `what`
/// names the file in the error.
fn apply_to(text: &str, patch: &JsonValue, what: &str) -> Result<String> {
    let Some(patch) = patch.as_object() else {
        bail!("expected a JSON object");
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("reading {what} as TOML"))?;
    merge_table(doc.as_table_mut(), patch)?;
    let text = doc.to_string();
    if let Err(e) = Config::from_toml(&text) {
        bail!("the change would make {what} invalid: {e}");
    }
    Ok(text)
}

/// Merges `patch` into a standard table: the document's, or one with a
/// `[header]`. A table added here gets a blank line before it, as a person
/// writes them, unless this table is empty and starts the file.
fn merge_table(table: &mut Table, patch: &Map<String, JsonValue>) -> Result<()> {
    for (key, value) in patch {
        match value {
            JsonValue::Null => {
                table.remove(key);
            }
            JsonValue::Object(object) => match table.get_mut(key) {
                Some(Item::Table(existing)) => merge_table(existing, object)?,
                Some(Item::Value(Value::InlineTable(existing))) => {
                    merge_inline_table(existing, object)?
                }
                Some(item) => {
                    let mut new = Table::new();
                    merge_table(&mut new, object)?;
                    *item = Item::Table(new);
                }
                None => {
                    let mut new = Table::new();
                    merge_table(&mut new, object)?;
                    // A patch of only `null`s for keys that aren't there
                    // leaves no table to add.
                    if !new.is_empty() {
                        if !table.is_empty() {
                            new.decor_mut().set_prefix("\n");
                        }
                        table.insert(key, Item::Table(new));
                    }
                }
            },
            _ => match table.get_mut(key) {
                Some(item) => replace_value(item, to_value(value)?),
                None => {
                    table.insert(key, Item::Value(to_value(value)?));
                }
            },
        }
    }
    Ok(())
}

/// Merges `patch` into an inline table (`{ a = 1 }`).
fn merge_inline_table(table: &mut InlineTable, patch: &Map<String, JsonValue>) -> Result<()> {
    for (key, value) in patch {
        match value {
            JsonValue::Null => {
                table.remove(key);
            }
            JsonValue::Object(object) => match table.get_mut(key) {
                Some(Value::InlineTable(existing)) => merge_inline_table(existing, object)?,
                Some(existing) => {
                    let mut new = InlineTable::new();
                    merge_inline_table(&mut new, object)?;
                    if new.is_empty() {
                        continue;
                    }
                    let decor = existing.decor().clone();
                    let mut value = Value::InlineTable(new);
                    *value.decor_mut() = decor;
                    *existing = value;
                }
                None => {
                    let mut new = InlineTable::new();
                    merge_inline_table(&mut new, object)?;
                    if new.is_empty() {
                        continue;
                    }
                    insert_inline(table, key, Value::InlineTable(new));
                }
            },
            _ => insert_inline(table, key, to_value(value)?),
        }
    }
    Ok(())
}

/// Sets `key` to `value` in an inline table, keeping a replaced value's
/// decor. A new key takes the place of the space before the closing brace,
/// so the comma it adds doesn't come after that space.
fn insert_inline(table: &mut InlineTable, key: &str, mut value: Value) {
    match table.get(key) {
        Some(old) => *value.decor_mut() = old.decor().clone(),
        None => {
            if let Some((_, last)) = table.iter_mut().last() {
                last.decor_mut().set_suffix("");
            }
        }
    }
    table.insert(key, value);
}

/// Replaces `item`'s value with `value`, keeping the key's place and the
/// old value's decor (a trailing comment, and the space after `=`).
fn replace_value(item: &mut Item, mut value: Value) {
    if let Some(old) = item.as_value() {
        *value.decor_mut() = old.decor().clone();
    }
    *item = Item::Value(value);
}

/// `value` as a TOML value: an object is an inline table, whose members
/// may not be `null`, as in TOML.
fn to_value(value: &JsonValue) -> Result<Value> {
    Ok(match value {
        JsonValue::Null => bail!("null in an array"),
        JsonValue::Bool(b) => Value::from(*b),
        JsonValue::String(s) => Value::from(s.as_str()),
        JsonValue::Number(n) => number(n)?,
        JsonValue::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_value(item)?);
            }
            Value::Array(array)
        }
        JsonValue::Object(object) => {
            let mut table = InlineTable::new();
            for (key, item) in object {
                table.insert(key, to_value(item)?);
            }
            Value::InlineTable(table)
        }
    })
}

/// A JSON number as a TOML one: an integer when it fits `i64`, else a
/// float. A `u64` above `i64::MAX` is an error, since TOML integers are
/// signed and it would come back changed.
fn number(n: &serde_json::Number) -> Result<Value> {
    if let Some(i) = n.as_i64() {
        return Ok(Value::from(i));
    }
    if n.as_u64().is_some() {
        bail!("{n} is above the largest TOML integer");
    }
    match n.as_f64() {
        Some(f) if f.is_finite() => Ok(Value::from(f)),
        _ => bail!("{n} is not a TOML number"),
    }
}

/// `value` as JSON. A datetime becomes its TOML text, such as
/// `1979-05-27T07:32:00Z`, since JSON has none.
fn to_json(value: &toml::Value) -> JsonValue {
    match value {
        toml::Value::String(s) => JsonValue::from(s.as_str()),
        toml::Value::Integer(i) => JsonValue::from(*i),
        toml::Value::Float(f) => JsonValue::from(*f),
        toml::Value::Boolean(b) => JsonValue::from(*b),
        toml::Value::Datetime(d) => JsonValue::from(d.to_string()),
        toml::Value::Array(a) => JsonValue::Array(a.iter().map(to_json).collect()),
        toml::Value::Table(t) => {
            JsonValue::Object(t.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

/// Creates `dir` and its parents when missing, mode 0700 on Unix. An
/// existing dir keeps the mode it has.
fn create_dir(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Writes `text` over `path` through a temporary file in the same dir, so
/// a reader sees the old file or the new one and never half of either.
/// The file is 0600 on Unix.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let (temp, mut file) = create_temp(dir)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temp, path)
        });
    match written {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e).with_context(|| format!("writing {}", path.display()))
        }
    }
}

/// Counts temporary files, so two writes in one process, or two processes
/// whose ids differ, don't pick the same name.
static TEMP_FILE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A new, empty file in `dir` to write the config into, and its path.
fn create_temp(dir: &Path) -> Result<(PathBuf, std::fs::File)> {
    for _ in 0..100 {
        let n = TEMP_FILE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!(
            ".{DEFAULT_CONFIG_FILE}.{}.{n}.tmp",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("writing in {}", dir.display()));
            }
        }
    }
    bail!(
        "could not find an unused temporary file name in {}",
        dir.display()
    );
}

#[cfg(test)]
mod tests {
    use super::{apply, read, refuse_legacy, set, to_value, DEFAULT_CONFIG_FILE};
    use serde_json::{json, Map};
    use std::path::{Path, PathBuf};

    /// A file as a person writes it: a comment at the top, a comment on a
    /// key, and an order of its own.
    const FILE: &str = "\
# Ganymede's settings, shared by the app and the server.
# The notebooks this server serves.
root_dir = \"/srv/notebooks\"
port = 8888
# Notebook tools for agents at /mcp.
mcp_enabled = false
";

    /// `text` as a `ganymede.toml` in a temp dir, with the dir.
    fn temp_file(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DEFAULT_CONFIG_FILE);
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    /// The names of everything in `dir`.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn setting_a_key_changes_only_its_line() {
        let out = apply(FILE, &json!({"mcp_enabled": true})).unwrap();
        assert_eq!(
            out,
            "\
# Ganymede's settings, shared by the app and the server.
# The notebooks this server serves.
root_dir = \"/srv/notebooks\"
port = 8888
# Notebook tools for agents at /mcp.
mcp_enabled = true
"
        );
    }

    #[test]
    fn a_new_key_goes_at_the_end() {
        let out = apply(FILE, &json!({"mcp_images": true})).unwrap();
        assert_eq!(out, format!("{FILE}mcp_images = true\n"));
    }

    #[test]
    fn a_replaced_key_keeps_its_comment_and_place() {
        let text = "# the port\nport = 8888   # the old one\nip = \"127.0.0.1\"\n";
        let out = apply(text, &json!({"port": 9000})).unwrap();
        assert_eq!(
            out,
            "# the port\nport = 9000   # the old one\nip = \"127.0.0.1\"\n"
        );
    }

    #[test]
    fn a_new_table_goes_at_the_end() {
        let text = "# The notebooks this server serves.\nroot_dir = \"/srv/notebooks\"\n";
        let out = apply(text, &json!({"tailscale": {"allow": ["a@example.com"]}})).unwrap();
        assert_eq!(
            out,
            "\
# The notebooks this server serves.
root_dir = \"/srv/notebooks\"

[tailscale]
allow = [\"a@example.com\"]
"
        );
    }

    #[test]
    fn a_new_table_in_an_empty_file_gets_no_blank_line() {
        let out = apply("", &json!({"tailscale": {"allow": []}})).unwrap();
        assert_eq!(out, "[tailscale]\nallow = []\n");
    }

    #[test]
    fn a_new_subtable_is_a_standard_table() {
        let out = apply(
            "port = 8888\n",
            &json!({"static_headers": {"/a": {"X": "y"}}}),
        )
        .unwrap();
        assert_eq!(
            out,
            "port = 8888\n\n[static_headers]\n\n[static_headers.\"/a\"]\nX = \"y\"\n"
        );
    }

    #[test]
    fn a_table_that_exists_is_merged_into() {
        let text = "port = 8888\n\n# Tailnet logins.\n[tailscale]\nwhois = false\n";
        let out = apply(text, &json!({"tailscale": {"allow": ["a@example.com"]}})).unwrap();
        assert_eq!(
            out,
            "port = 8888\n\n# Tailnet logins.\n[tailscale]\nwhois = false\nallow = [\"a@example.com\"]\n"
        );
    }

    #[test]
    fn an_inline_table_is_merged_into_itself() {
        let text = "static_headers = { \"/a\" = { X = \"y\" } }\n";
        let out = apply(text, &json!({"static_headers": {"/a": {"Z": "w"}}})).unwrap();
        assert_eq!(
            out,
            "static_headers = { \"/a\" = { X = \"y\", Z = \"w\" } }\n"
        );
    }

    #[test]
    fn a_host_grant_changes_alone() {
        let text = "\
root_dir = \"/srv/notebooks\"

[host]
read = [\"journal\"]
edit_self = true
";
        let out = apply(text, &json!({"host": {"edit_self": false}})).unwrap();
        assert_eq!(
            out,
            "\
root_dir = \"/srv/notebooks\"

[host]
read = [\"journal\"]
edit_self = false
"
        );
    }

    #[test]
    fn null_removes_a_key() {
        let text = "# the port\nport = 8888\nroot_dir = \"/srv/notebooks\"\n";
        let out = apply(text, &json!({"port": null})).unwrap();
        assert_eq!(out, "root_dir = \"/srv/notebooks\"\n");
    }

    #[test]
    fn null_removes_a_table_key() {
        let text = "[tailscale]\n# Who may in, without the token.\nallow = [\"a@example.com\"]\nwhois = false\n";
        let out = apply(text, &json!({"tailscale": {"allow": null}})).unwrap();
        assert_eq!(out, "[tailscale]\nwhois = false\n");
    }

    #[test]
    fn null_for_a_key_that_is_not_there_changes_nothing() {
        let out = apply(FILE, &json!({"ip": null, "host": {"run": null}})).unwrap();
        assert_eq!(out, FILE);
    }

    #[test]
    fn a_refused_edit_leaves_the_file_as_it_was() {
        let (dir, path) = temp_file(FILE);
        let before = std::fs::read(&path).unwrap();
        let cases = [
            (json!({"no_such_key": 1}), "unknown field `no_such_key`"),
            (
                json!({"host": {"no_such_key": true}}),
                "unknown field `no_such_key`",
            ),
            (json!({"port": "x"}), "invalid type"),
            (
                json!({"allow_origins": ["https://a.example", null]}),
                "null in an array",
            ),
            (json!(["port"]), "expected a JSON object"),
            (
                json!({"port": 18_446_744_073_709_551_615u64}),
                "above the largest TOML integer",
            ),
        ];
        for (patch, message) in cases {
            let err = set(Some(dir.path()), &patch).unwrap_err();
            let shown = format!("{err:#}");
            assert!(shown.contains(message), "{patch}: {shown}");
            // The Config check names the file; a bad patch doesn't need to.
            if message.starts_with("unknown field") || message == "invalid type" {
                assert!(
                    shown.contains(&path.display().to_string()),
                    "{patch}: {shown}"
                );
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "{patch} changed the file"
            );
        }
        assert_eq!(
            entries(dir.path()),
            [DEFAULT_CONFIG_FILE],
            "a temp file is left"
        );
    }

    #[test]
    fn a_refused_edit_says_so_without_a_path() {
        let err = apply(FILE, &json!({"port": "x"})).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("the change would make the config invalid: TOML parse error"),
            "{err:#}"
        );
    }

    #[test]
    fn a_missing_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("missing").join("ganymede");
        assert_eq!(read(Some(&nested)).unwrap(), Map::new());
    }

    #[test]
    fn set_creates_the_dir_and_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a").join("b");
        set(Some(&dir), &json!({"mcp_enabled": true})).unwrap();
        let path = dir.join(DEFAULT_CONFIG_FILE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "mcp_enabled = true\n"
        );
        assert_eq!(entries(&dir), [DEFAULT_CONFIG_FILE]);
        #[cfg(unix)]
        {
            assert_eq!(mode(&dir), 0o700, "the new dir");
            assert_eq!(mode(&path), 0o600, "the new file");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_file_becomes_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = temp_file("port = 8888\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        set(Some(dir.path()), &json!({"port": 9000})).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "port = 9000\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_stays_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        std::fs::create_dir(&dir).unwrap();
        let target = tmp.path().join("dotfiles").join("ganymede.toml");
        std::fs::create_dir(tmp.path().join("dotfiles")).unwrap();
        std::fs::write(&target, "port = 8888\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(DEFAULT_CONFIG_FILE)).unwrap();
        set(Some(&dir), &json!({"port": 9000})).unwrap();
        assert!(std::fs::symlink_metadata(dir.join(DEFAULT_CONFIG_FILE))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "port = 9000\n");
        assert_eq!(entries(&dir), [DEFAULT_CONFIG_FILE], "a temp file is left");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join(DEFAULT_CONFIG_FILE);
        std::os::unix::fs::symlink(tmp.path().join("gone.toml"), &link).unwrap();
        let err = set(Some(tmp.path()), &json!({"port": 9000})).unwrap_err();
        assert!(format!("{err:#}").contains("resolving the link"), "{err:#}");
    }

    #[test]
    fn read_gives_the_file_as_json() {
        let text = "\
static_headers = { \"/kernel.html\" = { \"Cross-Origin-Opener-Policy\" = \"same-origin\" } }
allow_origins = [\"https://a.example\", \"https://b.example\"]
# Not a key Config knows: read returns what the file holds.
lab_started = 1979-05-27T07:32:00Z
";
        let (dir, _) = temp_file(text);
        assert_eq!(
            read(Some(dir.path())).unwrap(),
            json!({
                "static_headers": {"/kernel.html": {"Cross-Origin-Opener-Policy": "same-origin"}},
                "allow_origins": ["https://a.example", "https://b.example"],
                "lab_started": "1979-05-27T07:32:00Z",
            })
            .as_object()
            .unwrap()
            .clone()
        );
    }

    #[test]
    fn a_file_that_is_not_toml_is_an_error_naming_it() {
        let (dir, path) = temp_file("port = ");
        let err = read(Some(dir.path())).unwrap_err();
        let shown = format!("{err:#}");
        assert!(shown.contains(&path.display().to_string()), "{shown}");
        assert!(shown.contains("TOML parse error"), "{shown}");
    }

    #[test]
    fn a_write_is_refused_while_the_jupyter_file_is_read() {
        let new = Path::new("/home/me/.config/ganymede/ganymede.toml");
        let old = Path::new("/home/me/.jupyter/ganymede.toml");
        let err = refuse_legacy(new, Some(old)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "/home/me/.jupyter/ganymede.toml is in the Jupyter config dir; move it to \
             /home/me/.config/ganymede/ganymede.toml before changing settings"
        );
        refuse_legacy(new, None).unwrap();
        refuse_legacy(new, Some(new)).unwrap();
    }

    #[test]
    fn values_become_toml() {
        assert_eq!(to_value(&json!(1)).unwrap().to_string(), "1");
        assert_eq!(to_value(&json!(-2)).unwrap().to_string(), "-2");
        assert_eq!(to_value(&json!(1.5)).unwrap().to_string(), "1.5");
        assert_eq!(to_value(&json!(true)).unwrap().to_string(), "true");
        assert_eq!(to_value(&json!("x")).unwrap().to_string(), "\"x\"");
        assert_eq!(
            to_value(&json!(["a", {"b": 1}])).unwrap().to_string(),
            "[\"a\", { b = 1 }]"
        );
        assert!(to_value(&json!(u64::MAX)).is_err());
        assert_eq!(
            to_value(&json!([1, null])).unwrap_err().to_string(),
            "null in an array"
        );
    }
}
