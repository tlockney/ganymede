//! `ganymede update`: replace this binary with another release of itself,
//! as `site/install.sh` installs one.
//!
//! It finds the latest release from GitHub's `/releases/latest` redirect,
//! downloads this platform's archive and its `.sha256` over HTTPS, checks
//! the archive's SHA-256, unpacks `ganymede` beside the running binary,
//! runs it with `--version`, and only then renames it over the old one.
//! cdp-kernel, JupyterLab and ganymede-ui are left alone; it says how to
//! update the ones installed.

use crate::lab::pypi::save_checked;
use crate::ui::{sum_for, Releases};
use anyhow::{bail, Context, Result};
use clap::Args;
use ganymede_server::lab::compare_versions;
use std::cmp::Ordering;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where releases are downloaded from, unless `GANYMEDE_DOWNLOAD_BASE`
/// names a mirror laid out the same way (`<base>/<tag>/<file>`).
const DEFAULT_RELEASES: &str = "https://github.com/tlockney/ganymede/releases/download";
/// The rolling prerelease release.yml updates on every push to main.
const MAIN_TAG: &str = "main-build";
/// The largest archive downloaded. 0.9.0's are about 20 MB.
const MAX_ARCHIVE_BYTES: u64 = 256 << 20;
/// The largest binary unpacked.
const MAX_BINARY_BYTES: u64 = 512 << 20;
/// The largest `.sha256` read.
const MAX_SUM_BYTES: u64 = 4 << 10;
/// The targets each release has an archive for (release.yml's matrix).
const TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

#[derive(Args)]
pub struct UpdateArgs {
    /// Install this release instead of the latest. It may be older.
    #[arg(long, value_name = "X.Y.Z", conflicts_with = "main")]
    version: Option<String>,
    /// Install the latest build of main instead of a release.
    ///
    /// A main build updates to the latest main build without this; pass
    /// `--version` to go back to a release.
    #[arg(long)]
    main: bool,
    /// Only say whether a newer release is available.
    #[arg(long)]
    check: bool,
    /// Reinstall even when this version is already installed.
    #[arg(long)]
    force: bool,
}

/// What to install: a release's version, or the latest main build.
#[derive(Debug, PartialEq, Eq)]
enum Want {
    Release(String),
    Main,
}

pub fn run(args: UpdateArgs) -> Result<ExitCode> {
    let current = env!("GANYMEDE_VERSION");
    let exe = std::env::current_exe().context("finding this binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if let Some(why) = managed(&exe, env!("GANYMEDE_BUILD_PROFILE")) {
        bail!("{why}");
    }
    let target = asset_target(env!("GANYMEDE_BUILD_TARGET"))?;
    let releases = Releases::new("GANYMEDE_DOWNLOAD_BASE", DEFAULT_RELEASES)?;
    clean_old(&exe);

    let explicit_version = args.version.is_some();
    let want = if args.main || (args.version.is_none() && is_main_build(current)) {
        if !args.main {
            eprintln!(
                "ganymede {current} is a build of main, so this updates it to the latest \
                 main build; `ganymede update --version X.Y.Z` installs a release instead"
            );
        }
        Want::Main
    } else {
        let explicit = args.version.is_some();
        let version = match args.version {
            Some(v) => checked_version(v.trim_start_matches('v'))?,
            None => latest()?,
        };
        match (explicit, compare_versions(&version, current)) {
            (_, Ordering::Equal) if !args.force => {
                eprintln!("ganymede {current} is installed; --force reinstalls it");
                return Ok(ExitCode::SUCCESS);
            }
            (false, Ordering::Less) if !args.force => {
                eprintln!(
                    "ganymede {current} is newer than the latest release, {version}; \
                     `--version {version}` installs it"
                );
                return Ok(ExitCode::SUCCESS);
            }
            _ => {}
        }
        Want::Release(version)
    };

    if args.check {
        match &want {
            Want::Release(v) if explicit_version => {
                println!("ganymede {v} is available; `ganymede update --version {v}` installs it")
            }
            Want::Release(v) => {
                println!("ganymede {v} is available; `ganymede update` installs it")
            }
            Want::Main => println!(
                "a main build's version is only known once it is downloaded; \
                 `ganymede update --main` installs the latest"
            ),
        }
        return Ok(ExitCode::SUCCESS);
    }

    let (tag, name, what) = match &want {
        Want::Release(v) => (
            format!("v{v}"),
            format!("ganymede-{v}-{target}"),
            format!("Ganymede {v}"),
        ),
        Want::Main => (
            MAIN_TAG.to_string(),
            format!("ganymede-main-{target}"),
            "the latest main build".to_string(),
        ),
    };
    let windows = target.contains("windows");
    let file = format!("{name}.{}", if windows { "zip" } else { "tar.gz" });
    let url = releases.asset(&tag, &file);
    let retry = match want {
        Want::Main => "; a new main build may be uploading, try again in a minute",
        Want::Release(_) => "",
    };

    let sha256 = {
        let sum_url = format!("{url}.sha256");
        let mut text = String::new();
        releases
            .get(&sum_url)
            .with_context(|| format!("download failed{retry}"))?
            .into_body()
            .into_reader()
            .take(MAX_SUM_BYTES)
            .read_to_string(&mut text)
            .with_context(|| format!("reading {sum_url}"))?;
        sum_for(&text, &file).with_context(|| format!("{sum_url} has no SHA-256 for {file}"))?
    };
    eprintln!("downloading {what} for {target}: {url} (sha256 {sha256})");
    let mut archive = tempfile::tempfile().context("creating a temporary file")?;
    let resp = releases
        .get(&url)
        .with_context(|| format!("download failed{retry}"))?;
    if let Some(len) = resp.body().content_length() {
        if len > MAX_ARCHIVE_BYTES {
            bail!("{url} is {len} bytes, more than the {MAX_ARCHIVE_BYTES} allowed");
        }
    }
    save_checked(
        resp.into_body().into_reader(),
        &mut archive,
        MAX_ARCHIVE_BYTES,
        &sha256,
    )
    .with_context(|| format!("{url}{retry}"))?;
    archive.rewind()?;

    // Unpack beside the binary, so the last step is a rename on one
    // file system.
    let dir = exe.parent().context("the binary has no directory")?;
    let mut new = tempfile::Builder::new()
        .prefix(".ganymede-update-")
        .tempfile_in(dir)
        .with_context(|| {
            format!(
                "can't write to {}; run `ganymede update` as a user who can, \
                 or reinstall with install.sh",
                dir.display()
            )
        })?;
    let entry = format!(
        "{name}/{}",
        if windows { "ganymede.exe" } else { "ganymede" }
    );
    if windows {
        unzip_one(archive, &entry, new.as_file_mut())?;
    } else {
        untar_one(archive, &entry, new.as_file_mut())?;
    }
    // Closed, so Linux will run it.
    let new = new.into_temp_path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }

    // Run it before it replaces anything: a binary for the wrong system,
    // or one missing a library, fails here and the old one stays.
    let got = trial_run(&new)?;
    match &want {
        Want::Release(v) if got != format!("ganymede {v}") => {
            bail!("the downloaded binary reports {got:?}, not ganymede {v}")
        }
        Want::Main if !got.starts_with("ganymede ") => {
            bail!("the downloaded binary reports {got:?}, not a ganymede version")
        }
        Want::Main if got == format!("ganymede {current}") && !args.force => {
            eprintln!("ganymede {current} is the latest main build; --force reinstalls it");
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }

    replace(new, &exe)?;
    eprintln!(
        "updated ganymede {current} to {} at {}",
        got.trim_start_matches("ganymede "),
        exe.display()
    );
    let new_base = base_version(got.trim_start_matches("ganymede "));
    if want == Want::Main && compare_versions(new_base, base_version(current)) == Ordering::Less {
        eprintln!(
            "this main build is from before release {}: a release isn't followed by a \
             main build until the next push to main",
            base_version(current)
        );
    }
    for hint in hints() {
        eprintln!("{hint}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Why `exe` should be updated some other way, if it should: it was
/// built from source, or a package manager put it there.
fn managed(exe: &Path, profile: &str) -> Option<String> {
    let names: Vec<String> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let has = |a: &str, b: &str| names.windows(2).any(|w| w[0] == a && w[1] == b);
    let parent = names.iter().rev().nth(1).map(String::as_str);
    let source = "update it from source: cargo install --git https://github.com/tlockney/ganymede ganymede-cli";
    if profile != "release" {
        return Some(format!("this is a {profile} build; {source}"));
    }
    if names.iter().any(|n| n == "target") && matches!(parent, Some("release" | "debug")) {
        return Some(format!(
            "this binary is in a cargo target directory; {source}"
        ));
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(|h| PathBuf::from(h).join("bin"))
        .and_then(|b| b.canonicalize().ok());
    if has(".cargo", "bin") || cargo_home.is_some_and(|b| exe.parent() == Some(b.as_path())) {
        return Some(format!(
            "this binary was installed with cargo install; {source}"
        ));
    }
    if names.iter().any(|n| n == "cellar") {
        return Some("this binary is managed by Homebrew; update it with brew".into());
    }
    if has("nix", "store") {
        return Some("this binary is in the Nix store; update it with Nix".into());
    }
    None
}

/// The release target to download for a binary built for `built`: the
/// same one, except the native arm64 build under Rosetta, as install.sh
/// picks.
fn asset_target(built: &str) -> Result<&'static str> {
    let Some(t) = TARGETS.iter().copied().find(|t| *t == built) else {
        bail!(
            "releases have no prebuilt binary for {built}; update from source: \
             cargo install --git https://github.com/tlockney/ganymede ganymede-cli"
        );
    };
    if t == "x86_64-apple-darwin" && under_rosetta() {
        return Ok("aarch64-apple-darwin");
    }
    Ok(t)
}

fn under_rosetta() -> bool {
    cfg!(target_os = "macos")
        && Command::new("sysctl")
            .args(["-n", "sysctl.proc_translated"])
            .output()
            .is_ok_and(|o| o.stdout.trim_ascii() == b"1")
}

/// Whether `version` is a main build's (release.yml sets `+main.<commit>`).
fn is_main_build(version: &str) -> bool {
    version.contains("+main")
}

/// `version` without its `+build` part.
fn base_version(version: &str) -> &str {
    version.split('+').next().unwrap_or(version)
}

/// `version` if it is safe in a URL path: ASCII letters, digits, `.`, `-`.
fn checked_version(version: &str) -> Result<String> {
    if version.is_empty()
        || !version.starts_with(|c: char| c.is_ascii_digit())
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        bail!("{version:?} is not a release version (X.Y.Z)");
    }
    Ok(version.to_string())
}

/// The latest release's version, from where GitHub's
/// `<repo>/releases/latest` redirects: `.../releases/tag/vX.Y.Z`. Asked
/// of GitHub even with a mirror, as install.sh does.
fn latest() -> Result<String> {
    let github = Releases::at(DEFAULT_RELEASES)?;
    let repo = DEFAULT_RELEASES.trim_end_matches("/download");
    let url = format!("{repo}/latest");
    let location = github.location(&url)?;
    location
        .as_deref()
        .and_then(tag_version)
        .map(checked_version)
        .transpose()?
        .with_context(|| format!("{url} names no release; none may be published yet"))
}

/// The version in a `.../tag/vX.Y.Z` URL.
fn tag_version(url: &str) -> Option<&str> {
    url.rsplit_once("/tag/v").map(|(_, v)| v.trim())
}

/// Copies `entry` out of a `.tar.gz` into `out`, within [`MAX_BINARY_BYTES`].
fn untar_one(archive: impl Read, entry: &str, out: &mut impl Write) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for e in tar.entries().context("reading the archive")? {
        let e = e.context("reading the archive")?;
        if e.header().entry_type().is_file() && e.path()?.as_ref() == Path::new(entry) {
            return copy_limited(e, out);
        }
    }
    bail!("the archive has no {entry}")
}

/// Copies `entry` out of a `.zip` into `out`, within [`MAX_BINARY_BYTES`].
fn unzip_one(archive: impl Read + Seek, entry: &str, out: &mut impl Write) -> Result<()> {
    let mut zip = zip::ZipArchive::new(archive).context("reading the archive")?;
    let e = match zip.by_name(entry) {
        Ok(e) if e.is_file() => e,
        _ => bail!("the archive has no {entry}"),
    };
    copy_limited(e, out)
}

fn copy_limited(from: impl Read, out: &mut impl Write) -> Result<()> {
    let n = std::io::copy(&mut from.take(MAX_BINARY_BYTES + 1), out)?;
    if n > MAX_BINARY_BYTES {
        bail!("the binary is more than the {MAX_BINARY_BYTES} bytes allowed");
    }
    out.flush()?;
    Ok(())
}

/// What `<binary> --version` prints, or why it didn't run.
fn trial_run(binary: &Path) -> Result<String> {
    let out = Command::new(binary)
        .arg("--version")
        .output()
        .context("the downloaded ganymede doesn't run on this system")?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        bail!(
            "the downloaded ganymede doesn't run on this system: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(text)
}

/// Where Windows moves the binary that was running during an update:
/// `<exe>.<pid>.old`. The pid keeps it apart from an older one that a
/// server started before an earlier update may still be running from.
fn old_path(exe: &Path, pid: u32) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{pid}.old"));
    exe.with_file_name(name)
}

/// Removes the `<exe>.*.old` files earlier updates on Windows left
/// behind, except ones still running, which Windows won't delete.
fn clean_old(exe: &Path) {
    if !cfg!(windows) {
        return;
    }
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return;
    };
    let prefix = format!("{}.", name.to_string_lossy());
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(&prefix) && n.ends_with(".old") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Renames `new` over `exe`. On Unix a running binary can be replaced
/// outright. Windows won't replace one, but will rename it, so the old
/// one moves aside first ([`old_path`]), and a later update removes it.
fn replace(new: tempfile::TempPath, exe: &Path) -> Result<()> {
    if cfg!(windows) {
        let old = old_path(exe, std::process::id());
        std::fs::rename(exe, &old).with_context(|| format!("moving {} aside", exe.display()))?;
        if let Err(e) = new.persist(exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e.error).with_context(|| format!("replacing {}", exe.display()));
        }
        let _ = std::fs::remove_file(&old);
        return Ok(());
    }
    new.persist(exe)
        .map_err(|e| e.error)
        .with_context(|| format!("replacing {}", exe.display()))
}

/// How to update what install.sh and `ganymede lab`/`ui install` set up
/// beside the binary, for the ones that are there.
fn hints() -> Vec<&'static str> {
    let mut out = Vec::new();
    if on_path("cdp-kernel") {
        out.push(
            "cdp-kernel is updated separately: \
             npm install -g @tlockney/cdp-kernel && cdp-kernel install",
        );
    }
    if ganymede_server::lab::installs_dir()
        .is_some_and(|d| !ganymede_server::lab::installed(&d).is_empty())
    {
        out.push("a new release can default to a newer JupyterLab; `ganymede lab install` gets it");
    }
    if crate::ui::installs_dir().is_some_and(|d| !crate::ui::installed(&d).is_empty()) {
        out.push("a new release can default to a newer ganymede-ui; `ganymede ui install` gets it");
    }
    out
}

fn on_path(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| {
        ["", ".cmd", ".exe"]
            .iter()
            .any(|ext| d.join(format!("{cmd}{ext}")).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (name, body) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, name, *body).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn untars_the_binary() {
        let a = tar_gz(&[
            ("ganymede-1.0.0-t/README.md", b"readme"),
            ("ganymede-1.0.0-t/ganymede", b"\x7fELF"),
        ]);
        let mut out = Vec::new();
        untar_one(&a[..], "ganymede-1.0.0-t/ganymede", &mut out).unwrap();
        assert_eq!(out, b"\x7fELF");
        let err = untar_one(&a[..], "ganymede-2.0.0-t/ganymede", &mut Vec::new()).unwrap_err();
        assert!(
            err.to_string().contains("no ganymede-2.0.0-t/ganymede"),
            "{err}"
        );
    }

    #[test]
    fn unzips_the_binary() {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("ganymede-1.0.0-t/ganymede.exe", opts).unwrap();
        w.write_all(b"MZ").unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let mut out = Vec::new();
        unzip_one(
            Cursor::new(&bytes),
            "ganymede-1.0.0-t/ganymede.exe",
            &mut out,
        )
        .unwrap();
        assert_eq!(out, b"MZ");
        assert!(unzip_one(Cursor::new(&bytes), "x/ganymede.exe", &mut Vec::new()).is_err());
    }

    #[test]
    fn reads_the_latest_tag() {
        assert_eq!(
            tag_version("https://github.com/tlockney/ganymede/releases/tag/v0.9.0"),
            Some("0.9.0")
        );
        assert_eq!(
            tag_version("https://github.com/tlockney/ganymede/releases"),
            None
        );
    }

    #[test]
    fn checks_versions() {
        assert_eq!(checked_version("0.10.0").unwrap(), "0.10.0");
        assert!(checked_version("0.10.0-rc.1").is_ok());
        for bad in ["", "../x", "0.1.0/../../evil", "latest", "1.0?x=y"] {
            assert!(checked_version(bad).is_err(), "{bad}");
        }
        assert!(is_main_build("0.9.0+main.abc123"));
        assert!(!is_main_build("0.9.0"));
        assert_eq!(base_version("0.8.0+main.abc123"), "0.8.0");
        assert_eq!(base_version("0.9.0"), "0.9.0");
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Ordering::Greater);
    }

    #[test]
    fn knows_managed_installs() {
        let release = |p: &str| managed(Path::new(p), "release");
        assert_eq!(release("/home/u/.local/bin/ganymede"), None);
        assert_eq!(release("/usr/local/bin/ganymede"), None);
        assert!(release("/home/u/.cargo/bin/ganymede")
            .unwrap()
            .contains("cargo install"));
        assert!(release("/src/ganymede/target/release/ganymede")
            .unwrap()
            .contains("target"));
        assert!(release("/src/g/target/x86_64-unknown-linux-gnu/release/ganymede").is_some());
        assert!(release("/opt/homebrew/Cellar/ganymede/1.0/bin/ganymede")
            .unwrap()
            .contains("Homebrew"));
        assert!(release("/nix/store/abc-ganymede/bin/ganymede")
            .unwrap()
            .contains("Nix"));
        assert!(managed(Path::new("/home/u/.local/bin/ganymede"), "debug")
            .unwrap()
            .contains("debug build"));
    }

    #[test]
    fn picks_a_release_target() {
        assert_eq!(
            asset_target("aarch64-apple-darwin").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            asset_target("x86_64-unknown-linux-gnu").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        let err = asset_target("x86_64-unknown-linux-musl").unwrap_err();
        assert!(err.to_string().contains("from source"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn replaces_the_binary_and_runs_it_first() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ganymede");
        std::fs::write(&exe, "old").unwrap();
        let mut new = tempfile::Builder::new().tempfile_in(dir.path()).unwrap();
        new.write_all(b"#!/bin/sh\necho ganymede 9.9.9\n").unwrap();
        let new = new.into_temp_path();
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(trial_run(&new).unwrap(), "ganymede 9.9.9");
        replace(new, &exe).unwrap();
        assert_eq!(trial_run(&exe).unwrap(), "ganymede 9.9.9");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_that_fails_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad");
        std::fs::write(&bad, "#!/bin/sh\necho 'not found' >&2; exit 127\n").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = trial_run(&bad).unwrap_err();
        assert!(
            err.to_string()
                .contains("doesn't run on this system: not found"),
            "{err}"
        );
    }

    #[test]
    fn old_binary_path() {
        assert_eq!(
            old_path(Path::new("/b/ganymede.exe"), 42),
            Path::new("/b/ganymede.exe.42.old")
        );
    }
}
