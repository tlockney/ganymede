//! `ganymede lab extension`: prebuilt JupyterLab extensions from PyPI.
//!
//! A package that ships a prebuilt extension puts it in its wheel under
//! `<name>.data/data/share/jupyter/labextensions/`, the layout `serve`
//! reads. Only that part of the wheel is unpacked, into
//! [`extensions_dir`], which `ganymede serve` always searches.
//!
//! Dependencies are followed only as far as the frontend needs them. A
//! requirement (from `Requires-Dist`, as PyPI's JSON lists it, without
//! `extra` markers) is looked at only when the package requiring it is
//! classified `Framework :: Jupyter`, and its wheel is installed only when
//! it is classified `Framework :: Jupyter :: JupyterLab :: Extensions`.
//! That is how `anywidget` brings `jupyterlab_widgets` through
//! `ipywidgets`, which has no extension of its own. Dependencies are
//! installed at their latest release; version specifiers are not resolved.

use super::{agent, download, pick_wheel, pypi_json};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use ganymede_server::lab::extensions_dir;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Installed by `ganymede lab install` unless `--no-default-extensions`:
/// the widget manager and anywidget, which cdp-kernel's widgets use.
pub const DEFAULTS: &[&str] = &["jupyterlab_widgets", "anywidget"];

/// Under [`extensions_dir`]: one JSON record per installed package, naming
/// the extension directories it put there.
const RECORDS: &str = ".packages";
const FRAMEWORK: &str = "Framework :: Jupyter";
const EXTENSIONS: &str = "Framework :: Jupyter :: JupyterLab :: Extensions";

#[derive(Subcommand)]
pub enum ExtensionCmd {
    /// Install packages' prebuilt extensions from their PyPI wheels, with
    /// the extension packages they depend on.
    Install {
        #[arg(required = true, value_name = "PACKAGE[==VERSION]")]
        packages: Vec<String>,
        /// Reinstall the named packages if they are already installed, and
        /// let them replace extensions another package installed.
        #[arg(long)]
        force: bool,
    },
    /// List installed packages and their extensions.
    List,
    /// Remove packages' extensions. Dependencies installed with them stay.
    Remove {
        #[arg(required = true)]
        packages: Vec<String>,
    },
}

pub fn run(cmd: ExtensionCmd) -> Result<ExitCode> {
    let root = root()?;
    match cmd {
        ExtensionCmd::Install { packages, force } => install(&root, &packages, force)?,
        ExtensionCmd::List => {
            let all = records(&root);
            if all.is_empty() {
                eprintln!(
                    "no extensions installed in {}; `ganymede lab extension install` adds them",
                    root.display()
                );
            }
            let w = all.iter().map(|r| r.name.len()).max().unwrap_or(0);
            let wv = all.iter().map(|r| r.version.len()).max().unwrap_or(0);
            for r in all {
                let via = match &r.required_by {
                    Some(p) => format!(" (for {p})"),
                    None => String::new(),
                };
                println!(
                    "{:<w$}  {:<wv$}  {}{via}",
                    r.name,
                    r.version,
                    r.extensions.join(", ")
                );
            }
        }
        ExtensionCmd::Remove { packages } => {
            for p in packages {
                let key = normalize(&p)?;
                let Some(rec) = record(&root, &key) else {
                    bail!("{p} is not installed in {}", root.display());
                };
                for ext in &rec.extensions {
                    remove_extension(&root, ext)?;
                }
                std::fs::remove_file(record_path(&root, &key))?;
                eprintln!("removed {} ({})", rec.name, rec.extensions.join(", "));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// [`DEFAULTS`], skipping those already installed.
pub fn install_defaults() -> Result<()> {
    let specs: Vec<String> = DEFAULTS.iter().map(|s| s.to_string()).collect();
    install(&root()?, &specs, false)
}

fn root() -> Result<PathBuf> {
    extensions_dir().context("no data directory; set GANYMEDE_DATA_DIR")
}

/// A package to look at: named on the command line, or required, directly
/// or not, by `required_by`, which was.
struct Want {
    name: String,
    version: Option<String>,
    required_by: Option<String>,
}

fn install(root: &Path, specs: &[String], force: bool) -> Result<()> {
    let mut queue = VecDeque::new();
    for s in specs {
        let (name, version) = parse_spec(s)?;
        queue.push_back(Want {
            name,
            version,
            required_by: None,
        });
    }
    let agent = agent();
    let mut seen = HashSet::new();
    while let Some(want) = queue.pop_front() {
        let key = normalize(&want.name)?;
        if !seen.insert(key.clone()) {
            continue;
        }
        let named = want.required_by.is_none();
        if let Some(r) = record(root, &key) {
            let same = want.version.as_ref().is_none_or(|v| *v == r.version);
            if !(named && force) && same {
                if named {
                    eprintln!(
                        "{} {} is already installed; --force reinstalls",
                        r.name, r.version
                    );
                }
                continue;
            }
        }
        let Some((api, meta)) = pypi_json(&agent, &want.name, want.version.as_deref())? else {
            match (named, &want.version) {
                (true, Some(v)) => bail!("{} {v} is not on PyPI", want.name),
                (true, None) => bail!("{} is not on PyPI", want.name),
                (false, _) => continue,
            }
        };
        let classifiers: Vec<&str> = meta["info"]["classifiers"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let has = |c: &str| {
            classifiers
                .iter()
                .any(|x| *x == c || x.starts_with(&format!("{c} :: ")))
        };
        if !named && !has(FRAMEWORK) {
            continue;
        }
        let name = meta["info"]["name"]
            .as_str()
            .unwrap_or(&want.name)
            .to_string();
        if named || has(EXTENSIONS) {
            let installed = install_wheel(
                &agent,
                root,
                &key,
                &name,
                &api,
                &meta,
                want.required_by.as_deref(),
                force,
            )?;
            if !installed && named {
                bail!("{name} has no prebuilt JupyterLab extension in its wheel");
            }
        }
        if has(FRAMEWORK) {
            for dep in requirements(&meta) {
                queue.push_back(Want {
                    name: dep,
                    version: None,
                    required_by: Some(want.required_by.clone().unwrap_or_else(|| name.clone())),
                });
            }
        }
    }
    Ok(())
}

/// Downloads the release's wheel, checks it, and moves its extensions
/// into `root`. False if the wheel has none. Fails, unless `force`, when
/// that would replace an extension another package installed.
#[allow(clippy::too_many_arguments)]
fn install_wheel(
    agent: &ureq::Agent,
    root: &Path,
    key: &str,
    name: &str,
    api: &str,
    meta: &Value,
    required_by: Option<&str>,
    force: bool,
) -> Result<bool> {
    let version = meta["info"]["version"].as_str().unwrap_or_default();
    let (url, sha256) =
        pick_wheel(meta).with_context(|| format!("no py3-none-any wheel for {name} in {api}"))?;
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    eprintln!("downloading {url}");
    let mut wheel = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(root)?;
    let got = download(agent, &url, wheel.as_file_mut())?;
    if got != sha256 {
        bail!("{url} has sha256 {got}, PyPI lists {sha256}");
    }
    let staging = tempfile::Builder::new()
        .prefix(&format!(".{key}-"))
        .tempdir_in(root)?;
    let extensions = extract(wheel.reopen()?, staging.path())?;
    if extensions.is_empty() {
        return Ok(false);
    }

    // A reinstall drops what the old version had and the new one doesn't;
    // an extension another package had installed now belongs to this one,
    // if `force` says so.
    let others: Vec<Record> = records(root).into_iter().filter(|r| r.key != key).collect();
    if !force {
        check_replacements(name, &others, &extensions)?;
    }
    if let Some(old) = record(root, key) {
        for ext in old.extensions.iter().filter(|e| !extensions.contains(*e)) {
            remove_extension(root, ext)?;
        }
    }
    for mut other in others {
        let before = other.extensions.len();
        other.extensions.retain(|e| !extensions.contains(e));
        if other.extensions.len() != before {
            eprintln!("{name} replaces extensions installed by {}", other.name);
            other.save(root)?;
        }
    }
    for ext in &extensions {
        let dest = root.join(ext);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("removing {}", dest.display()))?;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(staging.path().join(ext), &dest)
            .with_context(|| format!("moving {ext} to {}", dest.display()))?;
    }
    Record {
        key: key.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        extensions: extensions.into_iter().collect(),
        required_by: required_by.map(str::to_string),
    }
    .save(root)?;
    eprintln!("installed {name} {version}");
    Ok(true)
}

/// Fails if any of `others` installed one of `extensions`.
fn check_replacements(name: &str, others: &[Record], extensions: &BTreeSet<String>) -> Result<()> {
    for other in others {
        if let Some(ext) = other.extensions.iter().find(|e| extensions.contains(*e)) {
            bail!(
                "{name} would replace {ext}, installed by {}; --force replaces it",
                other.name
            );
        }
    }
    Ok(())
}

/// Unpacks `*.data/data/share/jupyter/labextensions/**` from a wheel into
/// `dest` and returns the extension names found: `name` or `@scope/name`.
/// Fails on a name [`valid_extension`] rejects.
pub fn extract(wheel: impl Read + Seek, dest: &Path) -> Result<BTreeSet<String>> {
    let mut zip = zip::ZipArchive::new(wheel).context("reading the wheel")?;
    let mut names = BTreeSet::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..`.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Some(parts) = path.iter().map(|p| p.to_str()).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let rest = match parts.as_slice() {
            [top, "data", "share", "jupyter", "labextensions", rest @ ..]
                if top.ends_with(".data") =>
            {
                rest
            }
            _ => continue,
        };
        // Files must sit inside an extension's directory.
        let depth = if rest[0].starts_with('@') { 2 } else { 1 };
        if rest.len() <= depth {
            continue;
        }
        let name = rest[..depth].join("/");
        if !valid_extension(&name) {
            bail!("the wheel has an extension with an invalid name: {name:?}");
        }
        names.insert(name);
        let out = dest.join(rest.iter().collect::<PathBuf>());
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f =
            std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
        std::io::copy(&mut entry, &mut f)
            .with_context(|| format!("unpacking {}", path.display()))?;
    }
    Ok(names)
}

/// An npm package name, `name` or `@scope/name`, safe to join to the root:
/// each part non-empty, of `[A-Za-z0-9._~-]`, not starting with `.` or `_`.
/// That rules out `.`, `..`, and [`RECORDS`].
fn valid_extension(ext: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with(['.', '_'])
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
    };
    match ext.split_once('/') {
        Some((scope, name)) => scope.strip_prefix('@').is_some_and(part) && part(name),
        None => part(ext),
    }
}

/// Removes `root/<ext>`, and its `@scope` directory once empty.
fn remove_extension(root: &Path, ext: &str) -> Result<()> {
    if !valid_extension(ext) {
        bail!("refusing to remove {ext:?}: not an extension name");
    }
    let dir = root.join(ext);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    if let Some(scope) = dir.parent().filter(|p| *p != root) {
        // Fails, harmlessly, while other extensions share the scope.
        let _ = std::fs::remove_dir(scope);
    }
    Ok(())
}

/// The names of the release's requirements, without those only needed
/// for an extra.
fn requirements(meta: &Value) -> Vec<String> {
    let Some(reqs) = meta["info"]["requires_dist"].as_array() else {
        return Vec::new();
    };
    reqs.iter()
        .filter_map(Value::as_str)
        .filter(|r| !r.split_once(';').is_some_and(|(_, m)| m.contains("extra")))
        .map(|r| {
            r.trim()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// `name` or `name==version`.
fn parse_spec(spec: &str) -> Result<(String, Option<String>)> {
    let (name, version) = match spec.split_once("==") {
        Some((n, v)) => (n.trim(), Some(v.trim().to_string())),
        None => (spec.trim(), None),
    };
    normalize(name).with_context(|| format!("{spec:?}: expected PACKAGE or PACKAGE==VERSION"))?;
    if let Some(v) = &version {
        let ok = !v.is_empty()
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '!' | '-' | '_'));
        if !ok {
            bail!("{spec:?}: invalid version {v:?}");
        }
    }
    Ok((name.to_string(), version))
}

/// The project name as PyPI compares names: lowercase, with runs of `-`,
/// `_`, and `.` as one `-`.
fn normalize(name: &str) -> Result<String> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && name.starts_with(|c: char| c.is_ascii_alphanumeric());
    if !ok {
        bail!("invalid package name {name:?}");
    }
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    Ok(out)
}

/// What an install put in [`extensions_dir`], kept in `.packages/<key>.json`.
struct Record {
    /// [`normalize`]d name.
    key: String,
    name: String,
    version: String,
    extensions: Vec<String>,
    /// The package named on the command line that brought this one in.
    required_by: Option<String>,
}

impl Record {
    /// None for a record naming anything [`valid_extension`] rejects, so a
    /// forged record cannot point a removal outside the root.
    fn from_json(key: &str, v: &Value) -> Option<Record> {
        let extensions = v["extensions"]
            .as_array()?
            .iter()
            .map(|e| {
                e.as_str()
                    .filter(|e| valid_extension(e))
                    .map(str::to_string)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Record {
            key: key.to_string(),
            name: v["name"].as_str()?.to_string(),
            version: v["version"].as_str()?.to_string(),
            extensions,
            required_by: v["required_by"].as_str().map(str::to_string),
        })
    }

    fn save(&self, root: &Path) -> Result<()> {
        let path = record_path(root, &self.key);
        std::fs::create_dir_all(root.join(RECORDS))?;
        let v = json!({
            "name": self.name,
            "version": self.version,
            "extensions": self.extensions,
            "required_by": self.required_by,
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&v)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

fn record_path(root: &Path, key: &str) -> PathBuf {
    root.join(RECORDS).join(format!("{key}.json"))
}

fn record(root: &Path, key: &str) -> Option<Record> {
    let bytes = std::fs::read(record_path(root, key)).ok()?;
    Record::from_json(key, &serde_json::from_slice(&bytes).ok()?)
}

/// Every record, by name.
fn records(root: &Path) -> Vec<Record> {
    let Ok(dir) = std::fs::read_dir(root.join(RECORDS)) else {
        return Vec::new();
    };
    let mut out: Vec<Record> = dir
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            record(root, file.strip_suffix(".json")?)
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn wheel(files: &[&str]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for f in files {
            w.start_file(*f, SimpleFileOptions::default()).unwrap();
            w.write_all(b"{}").unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_scoped_and_plain_extensions() {
        let ext = "w-1.0.data/data/share/jupyter/labextensions";
        let bytes = wheel(&[
            "w/__init__.py",
            "w-1.0.data/data/etc/jupyter/x.json",
            &format!("{ext}/@jupyter-widgets/jupyterlab-manager/package.json"),
            &format!("{ext}/@jupyter-widgets/jupyterlab-manager/static/remoteEntry.js"),
            &format!("{ext}/anywidget/package.json"),
            &format!("{ext}/anywidget/install.json"),
            &format!("{ext}/stray.json"),
            &format!("{ext}/@scope/stray.json"),
            "w-1.0.dist-info/METADATA",
        ]);
        let dir = tempfile::tempdir().unwrap();
        let names = extract(Cursor::new(bytes), dir.path()).unwrap();
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["@jupyter-widgets/jupyterlab-manager", "anywidget"]
        );
        let d = dir.path();
        assert!(d
            .join("@jupyter-widgets/jupyterlab-manager/static/remoteEntry.js")
            .is_file());
        assert!(d.join("anywidget/install.json").is_file());
        assert!(!d.join("stray.json").exists());
        assert!(!d.join("@scope").exists());
        assert!(!d.join("w").exists());
    }

    #[test]
    fn a_wheel_without_extensions_extracts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let names = extract(Cursor::new(wheel(&["w/__init__.py"])), dir.path()).unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn hostile_extension_names_are_refused() {
        let ext = "w-1.0.data/data/share/jupyter/labextensions";
        for bad in [
            ".packages/foo.json",
            ".packages/evil/package.json",
            "_x/package.json",
            "@scope/.hidden/package.json",
            "@.packages/x/package.json",
            "a b/package.json",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let bytes = wheel(&[&format!("{ext}/{bad}")]);
            assert!(extract(Cursor::new(bytes), dir.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn escaping_entries_are_skipped_and_symlinks_are_not_created() {
        let ext = "w-1.0.data/data/share/jupyter/labextensions";
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for f in [
            format!("{ext}/a/../../../../../escape.json"),
            format!("/{ext}/abs/package.json"),
            format!("{ext}/a/package.json"),
        ] {
            w.start_file(f, SimpleFileOptions::default()).unwrap();
            w.write_all(b"{}").unwrap();
        }
        w.add_symlink(
            format!("{ext}/a/link"),
            "/etc/passwd",
            SimpleFileOptions::default(),
        )
        .unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let names = extract(Cursor::new(bytes), &dest).unwrap();
        // A leading `/` is dropped, so the absolute entry stays in `dest`.
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "abs"]);
        assert!(dest.join("a/package.json").is_file());
        let link = std::fs::symlink_metadata(dest.join("a/link")).unwrap();
        assert!(!link.file_type().is_symlink());
        let mut found = Vec::new();
        for e in walk(parent.path()) {
            found.push(e.strip_prefix(parent.path()).unwrap().to_path_buf());
        }
        found.sort();
        assert_eq!(
            found,
            [
                PathBuf::from("dest/a/link"),
                PathBuf::from("dest/a/package.json"),
                PathBuf::from("dest/abs/package.json"),
            ]
        );
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn validates_extension_names() {
        for ok in [
            "anywidget",
            "@jupyter-widgets/jupyterlab-manager",
            "A.b~c_d-1",
        ] {
            assert!(valid_extension(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".packages",
            "_x",
            "@",
            "@/x",
            "@s/",
            "x/y",
            "@s/x/y",
            "../x",
            "@s/..",
            "/x",
            "a b",
            "a\\b",
        ] {
            assert!(!valid_extension(bad), "{bad}");
        }
    }

    #[test]
    fn forged_records_are_ignored_and_not_removed() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        std::fs::create_dir_all(root.join(RECORDS)).unwrap();
        for (key, ext) in [
            ("a", ".."),
            ("b", "../../home"),
            ("c", ".packages"),
            ("d", "/etc"),
        ] {
            std::fs::write(
                record_path(root, key),
                json!({"name": key, "version": "1", "extensions": ["ok", ext]}).to_string(),
            )
            .unwrap();
            assert!(record(root, key).is_none(), "{ext}");
        }
        assert!(records(root).is_empty());
        assert!(remove_extension(root, "..").is_err());
        assert!(remove_extension(root, ".packages").is_err());
        assert!(root.join(RECORDS).is_dir());
    }

    #[test]
    fn replacing_another_packages_extension_needs_force() {
        let other = Record {
            key: "jupyterlab-widgets".into(),
            name: "jupyterlab_widgets".into(),
            version: "3.0.17".into(),
            extensions: vec!["@jupyter-widgets/jupyterlab-manager".into()],
            required_by: None,
        };
        let mine: BTreeSet<String> = ["@jupyter-widgets/jupyterlab-manager".to_string()].into();
        let err = check_replacements("evil", std::slice::from_ref(&other), &mine)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("jupyterlab_widgets") && err.contains("--force"),
            "{err}"
        );
        let theirs: BTreeSet<String> = ["evil".to_string()].into();
        check_replacements("evil", &[other], &theirs).unwrap();
    }

    #[test]
    fn follows_requirements_without_extras() {
        let meta = json!({"info": {"requires_dist": [
            "ipywidgets>=7.6.0",
            "jupyterlab_widgets~=3.0.17",
            "colorama>=0.4.4; sys_platform == \"win32\"",
            "ipython[test] ; extra == \"test\"",
            "watchfiles>=1.1.0; python_version < \"3.12\" and extra == \"dev\"",
            "zope.interface (>=5)",
        ]}});
        assert_eq!(
            requirements(&meta),
            [
                "ipywidgets",
                "jupyterlab_widgets",
                "colorama",
                "zope.interface"
            ]
        );
        assert!(requirements(&json!({"info": {"requires_dist": null}})).is_empty());
    }

    #[test]
    fn parses_specs_and_normalizes_names() {
        assert_eq!(
            parse_spec("anywidget==0.9.18").unwrap(),
            ("anywidget".into(), Some("0.9.18".into()))
        );
        assert_eq!(parse_spec("anywidget").unwrap(), ("anywidget".into(), None));
        for bad in ["", "a>=1", "../x", "a==", "a==1/2", "-a"] {
            assert!(parse_spec(bad).is_err(), "{bad}");
        }
        assert_eq!(
            normalize("Jupyterlab_Widgets").unwrap(),
            "jupyterlab-widgets"
        );
        assert_eq!(normalize("a._-b").unwrap(), "a-b");
    }

    #[test]
    fn records_round_trip_and_extensions_are_removed_with_their_scope() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let rec = Record {
            key: "jupyterlab-widgets".into(),
            name: "jupyterlab_widgets".into(),
            version: "3.0.17".into(),
            extensions: vec!["@jupyter-widgets/jupyterlab-manager".into()],
            required_by: Some("anywidget".into()),
        };
        rec.save(root).unwrap();
        let back = records(root);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "jupyterlab_widgets");
        assert_eq!(back[0].extensions, rec.extensions);
        assert_eq!(back[0].required_by.as_deref(), Some("anywidget"));

        let ext = root.join("@jupyter-widgets/jupyterlab-manager");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("package.json"), "{}").unwrap();
        remove_extension(root, "@jupyter-widgets/jupyterlab-manager").unwrap();
        assert!(!root.join("@jupyter-widgets").exists());
    }
}
