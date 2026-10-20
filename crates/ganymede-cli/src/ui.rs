//! `ganymede ui`: the [ganymede-ui](https://github.com/tlockney/ganymede-ui)
//! notebook app, a static site, fetched from its GitHub releases.
//!
//! `ganymede ui install` downloads the release's zip over HTTPS, checks
//! its SHA-256 (pinned for [`DEFAULT_VERSION`], otherwise the line in the
//! release's `SHA256SUMS`), and unpacks it within [`Limits`] to
//! `ui/<version>/` under the data directory. `ganymede ui` serves the
//! newest install as `--static-dir`, as `ganymede lab` serves JupyterLab.

use crate::lab::pypi::{agent, https_host, save_checked};
use crate::lab::{replace_dir, version_dir, Limits};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use ganymede_server::lab::{compare_versions, data_dir};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The version `ganymede ui install` installs by default, and its zip's
/// SHA-256 from the release's `SHA256SUMS`. A mismatch fails the install.
pub const DEFAULT_VERSION: &str = "0.2.0";
const DEFAULT_SHA256: &str = "65bde2a82e2ef8fa15b4e2129824e5a1e8ae65e765bdf1c00020c09c89f17c8c";

/// Where releases are downloaded from, unless `GANYMEDE_UI_URL` names a
/// mirror laid out the same way (`<url>/v<version>/<file>`).
const DEFAULT_RELEASES: &str = "https://github.com/tlockney/ganymede-ui/releases/download";
/// Where GitHub redirects release downloads.
const ASSET_HOSTS: &[&str] = &[
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// The largest archive downloaded. 0.2.0's is about 2.4 MB.
const MAX_ARCHIVE_BYTES: u64 = 64 << 20;
/// The largest `SHA256SUMS` read.
const MAX_SUMS_BYTES: u64 = 64 << 10;

#[derive(Subcommand)]
pub enum UiCmd {
    /// Download a ganymede-ui release from GitHub and install it for
    /// `ganymede ui`.
    ///
    /// `GANYMEDE_UI_URL` points at a mirror of the release downloads.
    Install {
        /// ganymede-ui version, exactly.
        #[arg(long, default_value = DEFAULT_VERSION)]
        version: String,
        /// Reinstall if this version is already installed.
        #[arg(long)]
        force: bool,
    },
    /// List installed versions, newest first. `ganymede ui` serves the
    /// first.
    List {
        /// Print a JSON array of `{"version", "dir"}` objects instead.
        #[arg(long)]
        json: bool,
    },
    /// Remove installed versions.
    Remove {
        /// Versions, as `ganymede ui list` shows them.
        #[arg(required = true)]
        versions: Vec<String>,
    },
}

/// Where `ganymede ui install` keeps ganymede-ui, one directory per
/// version: `ui` under the data directory.
pub fn installs_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("ui"))
}

/// An installed version and its directory.
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub dir: PathBuf,
}

/// The versions installed under `root`, newest first.
pub fn installed(root: &Path) -> Vec<Installed> {
    let mut out: Vec<Installed> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let version = e.file_name().into_string().ok()?;
            let dir = e.path();
            (!version.starts_with('.') && dir.join("index.html").is_file())
                .then_some(Installed { version, dir })
        })
        .collect();
    out.sort_by(|a, b| compare_versions(&b.version, &a.version));
    out
}

/// The newest install, for `ganymede ui` to serve.
pub fn newest() -> Result<PathBuf> {
    let root = installs_dir().context("no data directory; set GANYMEDE_DATA_DIR")?;
    installed(&root)
        .into_iter()
        .next()
        .map(|i| i.dir)
        .context("no ganymede-ui installed; run `ganymede ui install` first")
}

pub fn run(cmd: UiCmd) -> Result<ExitCode> {
    let root = installs_dir().context("no data directory; set GANYMEDE_DATA_DIR")?;
    match cmd {
        UiCmd::Install { version, force } => {
            let dir = install(&root, &version, force)?;
            println!("{}", dir.display());
        }
        UiCmd::List { json: true } => {
            let all: Vec<_> = installed(&root)
                .into_iter()
                .map(|i| serde_json::json!({ "version": i.version, "dir": i.dir }))
                .collect();
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        UiCmd::List { json: false } => {
            let all = installed(&root);
            if all.is_empty() {
                eprintln!(
                    "no ganymede-ui installed in {}; `ganymede ui install` adds one",
                    root.display()
                );
            }
            let w = all.iter().map(|i| i.version.len()).max().unwrap_or(0);
            for i in all {
                println!("{:<w$}  {}", i.version, i.dir.display());
            }
        }
        UiCmd::Remove { versions } => {
            for v in versions {
                let dir = version_dir(&root, &v)?;
                if !dir.is_dir() {
                    bail!("ganymede-ui {v} is not installed in {}", root.display());
                }
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("removing {}", dir.display()))?;
                eprintln!("removed ganymede-ui {v} from {}", dir.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Release downloads: the base URL and the hosts allowed to serve them,
/// checked again after redirects.
struct Releases {
    base: String,
    hosts: Vec<String>,
    agent: ureq::Agent,
}

impl Releases {
    fn from_env() -> Result<Releases> {
        let base = std::env::var("GANYMEDE_UI_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_RELEASES.to_string());
        let base = base.trim_end_matches('/').to_string();
        let host = https_host(&base).with_context(|| format!("{base:?} (GANYMEDE_UI_URL)"))?;
        let mut hosts = vec![host];
        hosts.extend(ASSET_HOSTS.iter().map(|h| h.to_string()));
        Ok(Releases {
            base,
            hosts,
            agent: agent(),
        })
    }

    fn url(&self, version: &str, file: &str) -> String {
        format!("{}/v{version}/{file}", self.base)
    }

    /// GETs `url`, following up to five redirects by hand, each to an
    /// allowed host. Following them here, rather than in ureq, leaves each
    /// redirect's body unread: some proxies send GitHub's empty 302 body
    /// with a chunked framing ureq cannot drain.
    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut url = url.to_string();
        for _ in 0..=5 {
            self.check(&url)?;
            let resp = match self
                .agent
                .get(&url)
                .config()
                .max_redirects(0)
                .max_redirects_will_error(false)
                .build()
                .call()
            {
                Err(ureq::Error::StatusCode(404)) => bail!("{url}: not found"),
                r => r.with_context(|| format!("fetching {url}"))?,
            };
            if !resp.status().is_redirection() {
                return Ok(resp);
            }
            let next = resp
                .headers()
                .get(ureq::http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .with_context(|| format!("{url}: redirect without a location"))?;
            url = resolve(&url, next)?;
        }
        bail!("{url}: too many redirects")
    }

    fn check(&self, url: &str) -> Result<()> {
        let host = https_host(url)?;
        if !self.hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
            bail!(
                "{url}: host {host} is not allowed (allowed: {})",
                self.hosts.join(", ")
            );
        }
        Ok(())
    }

    /// The SHA-256 the release's `SHA256SUMS` lists for `file`.
    fn sha256(&self, version: &str, file: &str) -> Result<String> {
        let url = self.url(version, "SHA256SUMS");
        let mut text = String::new();
        self.get(&url)?
            .into_body()
            .into_reader()
            .take(MAX_SUMS_BYTES)
            .read_to_string(&mut text)
            .with_context(|| format!("reading {url}"))?;
        sum_for(&text, file).with_context(|| format!("{url} lists no {file}"))
    }
}

/// `location` resolved against `base`: absolute as is, or a path on
/// `base`'s origin.
fn resolve(base: &str, location: &str) -> Result<String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_string());
    }
    let uri: ureq::http::Uri = base.parse()?;
    if !location.starts_with('/') {
        bail!("{base}: unsupported redirect to {location:?}");
    }
    Ok(format!(
        "{}://{}{location}",
        uri.scheme_str().unwrap_or("https"),
        uri.authority().map(|a| a.as_str()).unwrap_or_default()
    ))
}

/// The hash on `file`'s line of a `sha256sum` listing.
fn sum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn install(root: &Path, version: &str, force: bool) -> Result<PathBuf> {
    let dest = version_dir(root, version)?;
    if !force && dest.join("index.html").is_file() {
        eprintln!("ganymede-ui {version} is already installed; --force reinstalls");
        return Ok(dest);
    }
    let releases = Releases::from_env()?;
    let file = format!("ganymede-ui-{version}.zip");
    let sha256 = if version == DEFAULT_VERSION {
        DEFAULT_SHA256.to_string()
    } else {
        releases.sha256(version, &file)?
    };
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;

    let url = releases.url(version, &file);
    eprintln!("downloading ganymede-ui {version}: {url} (sha256 {sha256})");
    let mut tmp = tempfile::Builder::new()
        .prefix(".download-")
        .tempfile_in(root)?;
    let resp = releases.get(&url)?;
    if let Some(len) = resp.body().content_length() {
        if len > MAX_ARCHIVE_BYTES {
            bail!("{url} is {len} bytes, more than the {MAX_ARCHIVE_BYTES} allowed");
        }
    }
    save_checked(
        resp.into_body().into_reader(),
        tmp.as_file_mut(),
        MAX_ARCHIVE_BYTES,
        &sha256,
    )
    .with_context(|| url.clone())?;

    let staging = tempfile::Builder::new()
        .prefix(&format!(".{version}-"))
        .tempdir_in(root)?;
    extract(tmp.reopen()?, staging.path(), &Limits::default())?;
    replace_dir(root, staging, &dest)?;
    eprintln!("installed ganymede-ui {version}");
    Ok(dest)
}

/// Unpacks a release zip into `dest`, without its one top-level directory
/// (`ganymede-ui-X.Y.Z/`). Fails if there is no `index.html` or the
/// archive exceeds `limits`.
pub fn extract(zip: impl Read + std::io::Seek, dest: &Path, limits: &Limits) -> Result<()> {
    let mut zip = zip::ZipArchive::new(zip).context("reading the archive")?;
    limits.check_entries(zip.len())?;
    let mut total = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..`.
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let rel: PathBuf = name.iter().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        limits.unpack(&mut entry, &out, &mut total)?;
    }
    if !dest.join("index.html").is_file() {
        bail!("the archive has no index.html");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn fixture_zip() -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        w.add_directory("ganymede-ui-9.9.9/", opts).unwrap();
        for (name, body) in [
            ("ganymede-ui-9.9.9/index.html", "<!doctype html>"),
            ("ganymede-ui-9.9.9/assets/index-abc.js", "app"),
            ("ganymede-ui-9.9.9/LICENSE", "MIT"),
            ("top-level-file", "dropped"),
            ("../escape.txt", "no"),
        ] {
            w.start_file(name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_without_the_top_directory() {
        let dir = tempfile::tempdir().unwrap();
        extract(Cursor::new(fixture_zip()), dir.path(), &Limits::default()).unwrap();
        let d = dir.path();
        assert_eq!(
            std::fs::read_to_string(d.join("index.html")).unwrap(),
            "<!doctype html>"
        );
        assert!(d.join("assets/index-abc.js").is_file());
        assert!(d.join("LICENSE").is_file());
        assert!(!d.join("top-level-file").exists());
        assert!(!d.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn rejects_an_archive_without_index_html() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        w.start_file("ganymede-ui-1.0.0/app.js", SimpleFileOptions::default())
            .unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();
        let err = extract(Cursor::new(bytes), dir.path(), &Limits::default()).unwrap_err();
        assert!(err.to_string().contains("index.html"), "{err}");
    }

    #[test]
    fn reads_sha256sums() {
        let a = "7f38895b54d972af655b25e4fc453b2c891da0f6d51ba78fe9ecc41c5795c289";
        let b = "96DE58E96961FFB67F5E40693EB587A8FDA6CC285E4AAC4B8E44B42709AC0C4F";
        let sums = format!("{a}  ganymede-ui-0.1.0.tar.gz\n{b} *ganymede-ui-0.1.0.zip\n");
        assert_eq!(
            sum_for(&sums, "ganymede-ui-0.1.0.zip").unwrap(),
            b.to_ascii_lowercase()
        );
        assert_eq!(sum_for(&sums, "ganymede-ui-0.1.0.tar.gz").unwrap(), a);
        assert_eq!(sum_for(&sums, "ganymede-ui-0.2.0.zip"), None);
        assert_eq!(
            sum_for("abc  ganymede-ui-0.1.0.zip", "ganymede-ui-0.1.0.zip"),
            None
        );
    }

    #[test]
    fn lists_installs_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        for v in ["0.1.0", "0.10.0", "0.2.0", ".0.3.0-staging"] {
            std::fs::create_dir_all(r.join(v)).unwrap();
            std::fs::write(r.join(v).join("index.html"), "").unwrap();
        }
        std::fs::create_dir_all(r.join("0.9.0")).unwrap(); // no index.html
        let versions: Vec<_> = installed(r).into_iter().map(|i| i.version).collect();
        assert_eq!(versions, ["0.10.0", "0.2.0", "0.1.0"]);
        assert!(installed(&r.join("missing")).is_empty());
    }

    #[test]
    fn resolves_redirects() {
        let base = "https://github.com/o/r/releases/download/v1/x.zip";
        assert_eq!(
            resolve(base, "https://objects.githubusercontent.com/a?b=c").unwrap(),
            "https://objects.githubusercontent.com/a?b=c"
        );
        assert_eq!(resolve(base, "/o/r/y").unwrap(), "https://github.com/o/r/y");
        assert!(resolve(base, "y").is_err());
    }

    #[test]
    fn only_allowed_hosts() {
        let r = Releases {
            base: DEFAULT_RELEASES.into(),
            hosts: vec!["github.com".into(), ASSET_HOSTS[0].into()],
            agent: agent(),
        };
        assert!(r.check(&r.url("0.1.0", "x.zip")).is_ok());
        assert!(r.check("https://objects.githubusercontent.com/a").is_ok());
        assert!(r.check("https://evil.example/x.zip").is_err());
        assert!(r.check("http://github.com/x.zip").is_err());
    }
}
