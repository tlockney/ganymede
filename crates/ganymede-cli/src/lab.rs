//! `ganymede lab`: JupyterLab's prebuilt frontend, fetched from PyPI.
//!
//! The built assets ship only in the `jupyterlab` wheel, which is a zip
//! file, so they are downloaded over HTTPS, checked against the SHA-256
//! PyPI publishes, and unpacked here. Nothing is executed: only
//! `<name>.data/data/share/jupyter/lab/{static,schemas,themes}` and the
//! license are kept. `ganymede serve` finds the result through
//! [`ganymede_server::lab::installs_dir`]. Prebuilt extensions come from
//! wheels the same way; see [`extension`].

mod extension;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use ganymede_server::lab::{installed, installs_dir};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The version `ganymede lab install` installs by default, and its wheel's
/// SHA-256 as PyPI published it. A mismatch for this version fails the
/// install even if PyPI's metadata agrees with the download.
pub const DEFAULT_VERSION: &str = "4.6.4";
const DEFAULT_SHA256: &str = "15b13f991d3985129c797eb84d9949eeb8b6615e14b444868e642411f2c418b2";

const PYPI: &str = "https://pypi.org/pypi";
/// The parts of JupyterLab's application directory the server reads.
const APP_DIRS: &[&str] = &["static", "schemas", "themes"];

#[derive(Subcommand)]
pub enum LabCmd {
    /// Download JupyterLab's static bundle from PyPI and install it for
    /// `ganymede serve`.
    Install {
        /// JupyterLab version.
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
    List,
    /// Remove installed versions.
    Remove {
        #[arg(required = true)]
        versions: Vec<String>,
    },
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
        LabCmd::List => {
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
    }
    Ok(ExitCode::SUCCESS)
}

/// `root/<version>`, refusing anything that is not a plain directory name.
fn version_dir(root: &Path, version: &str) -> Result<PathBuf> {
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
    let agent = agent();
    let (url, sha256) = wheel_url(&agent, version)?;
    if version == DEFAULT_VERSION && sha256 != DEFAULT_SHA256 {
        bail!("PyPI lists sha256 {sha256} for JupyterLab {version}, expected {DEFAULT_SHA256}");
    }
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;

    eprintln!("downloading {url}");
    let mut wheel = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(root)?;
    let got = download(&agent, &url, wheel.as_file_mut())?;
    if got != sha256 {
        bail!("{url} has sha256 {got}, PyPI lists {sha256}");
    }

    let staging = tempfile::Builder::new()
        .prefix(&format!(".{version}-"))
        .tempdir_in(root)?;
    extract(wheel.reopen()?, staging.path())?;
    if dest.exists() {
        std::fs::remove_dir_all(&dest).with_context(|| format!("removing {}", dest.display()))?;
    }
    std::fs::rename(staging.path(), &dest)
        .with_context(|| format!("moving the bundle to {}", dest.display()))?;
    let _ = staging.keep();
    eprintln!("installed JupyterLab {version}");
    Ok(dest)
}

/// HTTPS with the platform's trust store, so a proxy's CA installed there
/// is trusted, and `HTTPS_PROXY`/`ALL_PROXY` from the environment.
fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    let config = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent(concat!("ganymede/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::new_with_config(config)
}

/// The URL and SHA-256 of the version's wheel, from PyPI's JSON API.
fn wheel_url(agent: &ureq::Agent, version: &str) -> Result<(String, String)> {
    let (api, meta) = pypi_json(agent, "jupyterlab", Some(version))?
        .with_context(|| format!("JupyterLab {version} is not on PyPI"))?;
    pick_wheel(&meta).with_context(|| format!("no wheel for JupyterLab {version} in {api}"))
}

/// PyPI's JSON for a release, the latest when `version` is `None`, and the
/// URL it came from. `None` when PyPI has no such project or release.
fn pypi_json(
    agent: &ureq::Agent,
    project: &str,
    version: Option<&str>,
) -> Result<Option<(String, serde_json::Value)>> {
    let api = match version {
        Some(v) => format!("{PYPI}/{project}/{v}/json"),
        None => format!("{PYPI}/{project}/json"),
    };
    match agent.get(&api).call() {
        Ok(mut r) => {
            let meta = r
                .body_mut()
                .read_json()
                .with_context(|| format!("reading {api}"))?;
            Ok(Some((api, meta)))
        }
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("fetching {api}")),
    }
}

/// The release's pure wheel (`py3-none-any` or `py2.py3-none-any`): only
/// those carry the same data files on every platform.
fn pick_wheel(meta: &serde_json::Value) -> Option<(String, String)> {
    meta["urls"].as_array()?.iter().find_map(|u| {
        let file = u["filename"].as_str()?;
        if u["packagetype"] != "bdist_wheel" || !is_pure_wheel(file) {
            return None;
        }
        Some((
            u["url"].as_str()?.to_string(),
            u["digests"]["sha256"].as_str()?.to_ascii_lowercase(),
        ))
    })
}

/// Whether a wheel's tags (`name-version[-build]-pytag-abi-platform.whl`)
/// make it pure: any platform, no ABI, and a `py3` tag.
fn is_pure_wheel(file: &str) -> bool {
    let Some(stem) = file.strip_suffix(".whl") else {
        return false;
    };
    let tags: Vec<&str> = stem.rsplitn(4, '-').collect();
    match tags.as_slice() {
        [platform, abi, py, _] => {
            *platform == "any" && *abi == "none" && py.split('.').any(|t| t == "py3")
        }
        _ => false,
    }
}

/// Streams `url` into `out`, returning the body's SHA-256 in hex.
fn download(agent: &ureq::Agent, url: &str, out: &mut std::fs::File) -> Result<String> {
    let resp = agent
        .get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let mut body = resp.into_body().into_reader();
    let mut hash = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = body
            .read(&mut buf)
            .with_context(|| format!("reading {url}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    Ok(hex::encode(hash.finalize()))
}

/// Unpacks JupyterLab's application directory from a wheel into `dest`:
/// `*.data/data/share/jupyter/lab/{static,schemas,themes}/**` and the
/// `LICENSE` from `*.dist-info`. Fails if there is no `static/index.html`.
pub fn extract(wheel: impl Read + std::io::Seek, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(wheel).context("reading the wheel")?;
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
        let mut f =
            std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
        std::io::copy(&mut entry, &mut f)
            .with_context(|| format!("unpacking {}", name.display()))?;
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
    use std::io::Cursor;
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
        extract(Cursor::new(fixture_wheel()), dir.path()).unwrap();
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
        let err = extract(Cursor::new(bytes), dir.path()).unwrap_err();
        assert!(err.to_string().contains("static/index.html"), "{err}");
    }

    #[test]
    fn picks_the_wheel_from_pypi_metadata() {
        let meta = serde_json::json!({"urls": [
            {"packagetype": "sdist", "filename": "jupyterlab-1.0.tar.gz",
             "url": "https://x/s", "digests": {"sha256": "aa"}},
            {"packagetype": "bdist_wheel", "filename": "jupyterlab-1.0-cp312-cp312-win_amd64.whl",
             "url": "https://x/c", "digests": {"sha256": "cc"}},
            {"packagetype": "bdist_wheel", "filename": "jupyterlab-1.0-py3-none-any.whl",
             "url": "https://x/w", "digests": {"sha256": "BB"}},
        ]});
        assert_eq!(
            pick_wheel(&meta),
            Some(("https://x/w".to_string(), "bb".to_string()))
        );
        assert_eq!(pick_wheel(&serde_json::json!({"urls": []})), None);
    }

    #[test]
    fn recognises_pure_wheels() {
        for ok in [
            "a-1.0-py3-none-any.whl",
            "a-1.0-py2.py3-none-any.whl",
            "a-1.0-1-py3-none-any.whl",
        ] {
            assert!(is_pure_wheel(ok), "{ok}");
        }
        for bad in [
            "a-1.0-py2-none-any.whl",
            "a-1.0-cp312-cp312-win_amd64.whl",
            "a-1.0-cp312-abi3-any.whl",
            "a-1.0.tar.gz",
            "none-any.whl",
        ] {
            assert!(!is_pure_wheel(bad), "{bad}");
        }
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
