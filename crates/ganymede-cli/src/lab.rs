//! `ganymede lab`: JupyterLab's prebuilt frontend, fetched from PyPI.
//!
//! The built assets ship only in the `jupyterlab` wheel, which is a zip
//! file, so they are downloaded over HTTPS, checked against the SHA-256
//! PyPI publishes, and unpacked here, within the limits in [`pypi`] and
//! [`Limits`]. Nothing is executed: only
//! `<name>.data/data/share/jupyter/lab/{static,schemas,themes}` and the
//! license are kept. `ganymede serve` finds the result through
//! [`ganymede_server::lab::installs_dir`]. Prebuilt extensions come from
//! wheels the same way; see [`extension`].

mod extension;
pub(crate) mod pypi;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use ganymede_server::lab::{installed, installs_dir};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The version `ganymede lab install` installs by default, and its wheel's
/// SHA-256 as PyPI published it. A mismatch for this version fails the
/// install even if PyPI's metadata agrees with the download.
pub const DEFAULT_VERSION: &str = "4.6.4";
const DEFAULT_SHA256: &str = "15b13f991d3985129c797eb84d9949eeb8b6615e14b444868e642411f2c418b2";

/// The parts of JupyterLab's application directory the server reads.
const APP_DIRS: &[&str] = &["static", "schemas", "themes"];

#[derive(Subcommand)]
pub enum LabCmd {
    /// Download JupyterLab's static bundle from PyPI and install it for
    /// `ganymede serve`.
    ///
    /// `GANYMEDE_PYPI_URL` points at a mirror of PyPI's JSON API, and
    /// `GANYMEDE_PYPI_HOSTS` lists more hosts wheels may come from.
    Install {
        /// JupyterLab version, exactly. A yanked release is refused.
        #[arg(long, default_value = DEFAULT_VERSION)]
        version: String,
        /// Reinstall if this version is already installed.
        #[arg(long)]
        force: bool,
        /// Skip the extensions installed with JupyterLab by default:
        /// `jupyterlab_widgets` and `anywidget`, for widgets.
        #[arg(long)]
        no_default_extensions: bool,
    },
    /// Install, list, and remove prebuilt JupyterLab extensions.
    Extension {
        #[command(subcommand)]
        cmd: extension::ExtensionCmd,
    },
    /// List installed versions, newest first. `ganymede serve` uses the
    /// first unless told otherwise.
    List {
        /// Print a JSON array of `{"version", "dir"}` objects instead.
        #[arg(long)]
        json: bool,
    },
    /// Remove installed versions.
    Remove {
        /// Versions, as `ganymede lab list` shows them.
        #[arg(required = true)]
        versions: Vec<String>,
    },
    /// Delete what interrupted installs left behind: partial downloads and
    /// unpacked staging directories.
    Clean,
    /// Remove every installed JupyterLab version and extension, and the
    /// records of them, leaving nothing of `ganymede lab install` behind.
    Uninstall,
}

pub fn run(cmd: LabCmd) -> Result<ExitCode> {
    let root = installs_dir().context("no data directory; set GANYMEDE_DATA_DIR")?;
    match cmd {
        LabCmd::Install {
            version,
            force,
            no_default_extensions,
        } => {
            let dir = install(&root, &version, force)?;
            if !no_default_extensions {
                extension::install_defaults()?;
            }
            println!("{}", dir.display());
        }
        LabCmd::Extension { cmd } => return extension::run(cmd),
        LabCmd::List { json: true } => {
            let all: Vec<_> = installed(&root)
                .into_iter()
                .map(|i| serde_json::json!({ "version": i.version, "dir": i.dir }))
                .collect();
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        LabCmd::List { json: false } => {
            let all = installed(&root);
            if all.is_empty() {
                eprintln!(
                    "no JupyterLab installed in {}; `ganymede lab install` adds one",
                    root.display()
                );
            }
            let w = all.iter().map(|i| i.version.len()).max().unwrap_or(0);
            for i in all {
                println!("{:<w$}  {}", i.version, i.dir.display());
            }
        }
        LabCmd::Remove { versions } => {
            for v in versions {
                let dir = version_dir(&root, &v)?;
                if !dir.is_dir() {
                    bail!("JupyterLab {v} is not installed in {}", root.display());
                }
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("removing {}", dir.display()))?;
                eprintln!("removed JupyterLab {v} from {}", dir.display());
            }
        }
        LabCmd::Clean => {
            let mut n = 0;
            for dir in [Some(root.clone()), extension::dir()].into_iter().flatten() {
                n += clean(&dir)?;
            }
            eprintln!("removed {n} leftover files and directories");
        }
        LabCmd::Uninstall => {
            for dir in [Some(root.clone()), extension::dir()].into_iter().flatten() {
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)
                        .with_context(|| format!("removing {}", dir.display()))?;
                    eprintln!("removed {}", dir.display());
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Removes the dot-named entries in `dir` that installs create and rename
/// away on success (`.download-*`, `.<version>-*`, `.<package>-*`), but
/// not the extension records. Returns how many were removed.
fn clean(dir: &Path) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut n = 0;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') || name == extension::RECORDS {
            continue;
        }
        let path = e.path();
        let r = if e.file_type().is_ok_and(|t| t.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        r.with_context(|| format!("removing {}", path.display()))?;
        eprintln!("removed {}", path.display());
        n += 1;
    }
    Ok(n)
}

/// `root/<version>`, refusing anything that is not a plain directory name.
pub(crate) fn version_dir(root: &Path, version: &str) -> Result<PathBuf> {
    let ok = !version.is_empty()
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    if !ok {
        bail!("invalid version {version:?}");
    }
    Ok(root.join(version))
}

fn install(root: &Path, version: &str, force: bool) -> Result<PathBuf> {
    let dest = version_dir(root, version)?;
    if !force && dest.join("static/index.html").is_file() {
        eprintln!("JupyterLab {version} is already installed; --force reinstalls");
        return Ok(dest);
    }
    let index = pypi::Index::from_env()?;
    let (api, meta) = index
        .json("jupyterlab", Some(version))?
        .with_context(|| format!("JupyterLab {version} is not on PyPI"))?;
    pypi::check_not_yanked("JupyterLab", &meta)?;
    let wheel = pypi::pick_wheel(&meta["urls"])
        .with_context(|| format!("no wheel for JupyterLab {version} in {api}"))?;
    if version == DEFAULT_VERSION && wheel.sha256 != DEFAULT_SHA256 {
        bail!(
            "PyPI lists sha256 {} for JupyterLab {version}, expected {DEFAULT_SHA256}",
            wheel.sha256
        );
    }
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;

    eprintln!(
        "downloading JupyterLab {version}: {} (sha256 {})",
        wheel.url, wheel.sha256
    );
    let mut file = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(root)?;
    index.download(&wheel.url, &wheel.sha256, file.as_file_mut())?;

    let staging = tempfile::Builder::new()
        .prefix(&format!(".{version}-"))
        .tempdir_in(root)?;
    extract(file.reopen()?, staging.path(), &Limits::default())?;
    replace_dir(root, staging, &dest)?;
    eprintln!("installed JupyterLab {version}");
    Ok(dest)
}

/// Moves `staging` to `dest`. An existing `dest` is first renamed aside
/// and restored if the move fails, so an install is never left half gone.
pub(crate) fn replace_dir(root: &Path, staging: tempfile::TempDir, dest: &Path) -> Result<()> {
    // Dropped last, removing the old install from inside it.
    let aside = tempfile::Builder::new().prefix(".old-").tempdir_in(root)?;
    let old = aside.path().join("old");
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &old).with_context(|| format!("moving {} aside", dest.display()))?;
    }
    if let Err(e) = std::fs::rename(staging.path(), dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(e).with_context(|| format!("moving the bundle to {}", dest.display()));
    }
    let _ = staging.keep();
    Ok(())
}

/// Caps on what one wheel may unpack to, so a hostile or broken archive
/// cannot fill the disk. JupyterLab's bundle is about 4,000 files and
/// 60 MB.
pub struct Limits {
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 100_000,
            max_file_bytes: 256 << 20,
            max_total_bytes: 1 << 30,
        }
    }
}

impl Limits {
    /// Fails for an archive with more entries than allowed.
    pub fn check_entries(&self, n: usize) -> Result<()> {
        if n > self.max_entries {
            bail!(
                "the wheel has {n} entries, more than the {} allowed",
                self.max_entries
            );
        }
        Ok(())
    }

    /// Writes `entry` to a new file at `out`, adding its size to `total`.
    /// Fails, leaving the file partly written, past either cap.
    pub fn unpack(&self, entry: impl Read, out: &Path, total: &mut u64) -> Result<()> {
        let mut f =
            std::fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
        let n = std::io::copy(&mut entry.take(self.max_file_bytes + 1), &mut f)
            .with_context(|| format!("unpacking {}", out.display()))?;
        if n > self.max_file_bytes {
            bail!(
                "{} unpacks to more than the {} bytes allowed for one file",
                out.display(),
                self.max_file_bytes
            );
        }
        *total += n;
        if *total > self.max_total_bytes {
            bail!(
                "the wheel unpacks to more than the {} bytes allowed",
                self.max_total_bytes
            );
        }
        Ok(())
    }
}

/// Unpacks JupyterLab's application directory from a wheel into `dest`:
/// `*.data/data/share/jupyter/lab/{static,schemas,themes}/**` and the
/// `LICENSE` from `*.dist-info`. Fails if there is no `static/index.html`
/// or the wheel exceeds `limits`.
pub fn extract(wheel: impl Read + std::io::Seek, dest: &Path, limits: &Limits) -> Result<()> {
    let mut zip = zip::ZipArchive::new(wheel).context("reading the wheel")?;
    limits.check_entries(zip.len())?;
    let mut total = 0;
    let mut license = false;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..`.
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let Some(out) = destination(&name, &mut license) else {
            continue;
        };
        let out = dest.join(out);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        limits.unpack(&mut entry, &out, &mut total)?;
    }
    if !dest.join("static/index.html").is_file() {
        bail!("the wheel has no share/jupyter/lab/static/index.html");
    }
    Ok(())
}

/// Where a wheel entry goes, relative to the install directory, if it is
/// kept. The first `LICENSE*` in the `.dist-info` directory is the license.
fn destination(name: &Path, license_taken: &mut bool) -> Option<PathBuf> {
    let parts: Vec<&str> = name.iter().map(|p| p.to_str()).collect::<Option<_>>()?;
    match parts.as_slice() {
        [top, "data", "share", "jupyter", "lab", dir, rest @ ..]
            if top.ends_with(".data") && APP_DIRS.contains(dir) && !rest.is_empty() =>
        {
            Some(std::iter::once(*dir).chain(rest.iter().copied()).collect())
        }
        [top, .., file]
            if top.ends_with(".dist-info") && file.starts_with("LICENSE") && !*license_taken =>
        {
            *license_taken = true;
            Some(PathBuf::from("LICENSE"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    /// A wheel laid out as JupyterLab's is, plus files that must be left out.
    fn fixture_wheel() -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        let lab = "jupyterlab-9.9.9.data/data/share/jupyter/lab";
        for (name, body) in [
            ("jupyterlab/__init__.py", "x"),
            ("jupyterlab-9.9.9.data/data/etc/jupyter/x.json", "{}"),
            (&format!("{lab}/static/index.html"), "<html>"),
            (&format!("{lab}/static/package.json"), "{}"),
            (
                &format!("{lab}/schemas/@jupyterlab/a-extension/plugin.json"),
                "{}",
            ),
            (
                &format!("{lab}/themes/@jupyterlab/theme-light/index.css"),
                "a{}",
            ),
            (&format!("{lab}/staging/package.json"), "{}"),
            (
                "jupyterlab-9.9.9.dist-info/licenses/LICENSE",
                "BSD-3-Clause",
            ),
            ("jupyterlab-9.9.9.dist-info/METADATA", "Name: jupyterlab"),
            ("../escape.txt", "no"),
        ] {
            w.start_file(name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.add_directory(format!("{lab}/static/empty/"), opts)
            .unwrap();
        w.finish().unwrap().into_inner()
    }

    fn files(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(root).unwrap();
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn extracts_the_application_directory_and_license() {
        let dir = tempfile::tempdir().unwrap();
        extract(Cursor::new(fixture_wheel()), dir.path(), &Limits::default()).unwrap();
        assert_eq!(
            files(dir.path()),
            [
                "LICENSE",
                "schemas/@jupyterlab/a-extension/plugin.json",
                "static/index.html",
                "static/package.json",
                "themes/@jupyterlab/theme-light/index.css",
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("LICENSE")).unwrap(),
            "BSD-3-Clause"
        );
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn rejects_a_wheel_without_the_bundle() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("jupyterlab/__init__.py", SimpleFileOptions::default())
            .unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();
        let err = extract(Cursor::new(bytes), dir.path(), &Limits::default()).unwrap_err();
        assert!(err.to_string().contains("static/index.html"), "{err}");
    }

    #[test]
    fn oversized_wheels_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tight = |e, f, t| Limits {
            max_entries: e,
            max_file_bytes: f,
            max_total_bytes: t,
        };
        let err = extract(
            Cursor::new(fixture_wheel()),
            dir.path(),
            &tight(3, 1 << 20, 1 << 20),
        )
        .unwrap_err();
        assert!(err.to_string().contains("entries"), "{err}");
        // The kept files are 6, 2, 2, 3, and 12 bytes: 25 in all.
        let err = extract(
            Cursor::new(fixture_wheel()),
            dir.path(),
            &tight(100, 5, 1 << 20),
        )
        .unwrap_err();
        assert!(err.to_string().contains("for one file"), "{err}");
        let err = extract(
            Cursor::new(fixture_wheel()),
            dir.path(),
            &tight(100, 100, 20),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unpacks to more than"), "{err}");
    }

    #[test]
    fn a_reinstall_replaces_the_old_directory_whole() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("1.0");
        std::fs::create_dir_all(dest.join("static")).unwrap();
        std::fs::write(dest.join("static/old.js"), "old").unwrap();
        let staging = tempfile::tempdir_in(root.path()).unwrap();
        std::fs::write(staging.path().join("new.js"), "new").unwrap();
        replace_dir(root.path(), staging, &dest).unwrap();
        assert!(dest.join("new.js").is_file());
        assert!(!dest.join("static").exists());
        let left: Vec<_> = std::fs::read_dir(root.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "staging and the old copy are gone");
    }

    #[test]
    fn clean_removes_leftovers_and_keeps_installs() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        for d in [
            "4.6.4/static",
            ".4.6.4-abc/static",
            ".packages",
            "anywidget",
        ] {
            std::fs::create_dir_all(r.join(d)).unwrap();
        }
        std::fs::write(r.join(".download-xyz"), "partial").unwrap();
        std::fs::write(r.join(".packages/anywidget.json"), "{}").unwrap();
        assert_eq!(clean(r).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(r)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".packages", "4.6.4", "anywidget"]);
        assert_eq!(clean(&r.join("missing")).unwrap(), 0);
    }

    #[test]
    fn version_dirs_stay_inside_the_root() {
        let root = Path::new("/r");
        assert_eq!(version_dir(root, "4.6.4").unwrap(), root.join("4.6.4"));
        for bad in ["", "..", ".hidden", "4/5", "a\\b"] {
            assert!(version_dir(root, bad).is_err(), "{bad}");
        }
    }
}
