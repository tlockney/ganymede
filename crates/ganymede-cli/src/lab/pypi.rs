//! Fetching from PyPI for `ganymede lab install` and `lab extension
//! install`, with the limits an untrusted network calls for: HTTPS only,
//! timeouts, a host allowlist (checked after redirects), a download size
//! cap, yanked releases skipped, and PEP 440 version specifiers. A request
//! the server fails (5xx) or that times out is sent again a few times
//! ([`retry`]).
//!
//! What this does not give is provenance: a wheel's SHA-256 comes from the
//! same PyPI response as its URL, so it catches corruption, not a
//! malicious upload. An extension is JavaScript that runs on the server's
//! origin with the user's token; installing one trusts its publisher.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::time::Duration;
use ureq::ResponseExt;

/// PyPI's JSON API, unless `GANYMEDE_PYPI_URL` names a mirror.
const DEFAULT_INDEX: &str = "https://pypi.org/pypi";
/// Where PyPI serves wheels from.
const FILES_HOST: &str = "files.pythonhosted.org";
/// The largest wheel downloaded. JupyterLab's is about 12 MB.
pub const MAX_WHEEL_BYTES: u64 = 256 << 20;
/// The largest JSON response read. PyPI's for JupyterLab is a few MB.
const MAX_JSON_BYTES: u64 = 64 << 20;

/// The JSON API to ask and the hosts allowed to answer: the API's own
/// host, and for wheels also `files.pythonhosted.org` and any listed in
/// `GANYMEDE_PYPI_HOSTS` (comma separated).
pub struct Index {
    pub api: String,
    pub api_host: String,
    pub file_hosts: Vec<String>,
    agent: ureq::Agent,
}

impl Index {
    pub fn from_env() -> Result<Index> {
        let api = std::env::var("GANYMEDE_PYPI_URL")
            .ok()
            .filter(|s| !s.is_empty());
        let extra = std::env::var("GANYMEDE_PYPI_HOSTS").unwrap_or_default();
        Index::new(api.as_deref().unwrap_or(DEFAULT_INDEX), &extra)
    }

    pub fn new(api: &str, extra_hosts: &str) -> Result<Index> {
        let api = api.trim_end_matches('/').to_string();
        let api_host = https_host(&api)
            .with_context(|| format!("package index {api:?} (GANYMEDE_PYPI_URL)"))?;
        let mut file_hosts = vec![FILES_HOST.to_string(), api_host.clone()];
        file_hosts.extend(
            extra_hosts
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty()),
        );
        Ok(Index {
            api,
            api_host,
            file_hosts,
            agent: agent(),
        })
    }

    /// PyPI's JSON for a release, or for the project (its latest release
    /// and every release's files) when `version` is `None`, and the URL it
    /// came from. `None` when PyPI has no such project or release.
    /// Fails on a version that isn't a public PEP 440 version, so nothing
    /// but a version reaches the URL's path.
    pub fn json(&self, project: &str, version: Option<&str>) -> Result<Option<(String, Value)>> {
        let url = match version {
            Some(v) => {
                if !is_public_version(v) {
                    bail!(
                        "invalid version {v:?} for {project}: PyPI has only public \
                         versions (no local `+` part)"
                    );
                }
                format!("{}/{project}/{v}/json", self.api)
            }
            None => format!("{}/{project}/json", self.api),
        };
        match retry(&url, || self.agent.get(&url).call()) {
            Ok(mut r) => {
                check_host(
                    &r.get_uri().to_string(),
                    std::slice::from_ref(&self.api_host),
                )?;
                let meta = r
                    .body_mut()
                    .with_config()
                    .limit(MAX_JSON_BYTES)
                    .read_json()
                    .with_context(|| format!("reading {url}"))?;
                Ok(Some((url, meta)))
            }
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("fetching {url}")),
        }
    }

    /// Streams the wheel at `url` into `out`, failing unless it comes from
    /// an allowed host, is at most [`MAX_WHEEL_BYTES`], and has `sha256`.
    pub fn download(&self, url: &str, sha256: &str, out: &mut std::fs::File) -> Result<()> {
        check_host(url, &self.file_hosts)?;
        let resp =
            retry(url, || self.agent.get(url).call()).with_context(|| format!("fetching {url}"))?;
        // A redirect may only land on an allowed host too.
        check_host(&resp.get_uri().to_string(), &self.file_hosts)?;
        if let Some(len) = resp.body().content_length() {
            if len > MAX_WHEEL_BYTES {
                bail!("{url} is {len} bytes, more than the {MAX_WHEEL_BYTES} allowed");
            }
        }
        let body = resp.into_body().into_reader();
        save_checked(body, out, MAX_WHEEL_BYTES, sha256).with_context(|| url.to_string())
    }
}

/// HTTPS only, with the platform's trust store, so a proxy's CA installed
/// there is trusted, and `HTTPS_PROXY`/`ALL_PROXY` from the environment.
/// Timeouts keep a stalled server from hanging the install.
pub(crate) fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    let config = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .https_only(true)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(10 * 60)))
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .max_redirects(5)
        .user_agent(concat!("ganymede/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::new_with_config(config)
}

/// The wait before each retry of a request: three retries, seven seconds
/// of waiting in all.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// Whether a request that failed with `err` may succeed if sent again:
/// the server answered 5xx, or took the request and sent no response in
/// time. GitHub's release downloads and PyPI both answer 502 to 504 now
/// and then (#634). Anything else (404, a refused connection, a TLS
/// failure) would fail the same way again. So would a host that can't
/// be reached: retrying the connect timeout would keep someone on a
/// dead network waiting two minutes for the error.
fn is_transient(err: &ureq::Error) -> bool {
    matches!(
        err,
        ureq::Error::StatusCode(500..=599) | ureq::Error::Timeout(ureq::Timeout::RecvResponse)
    )
}

/// Sends the request for `url` with `call`, and again after each of
/// [`RETRY_DELAYS`] while it fails with a transient error
/// ([`is_transient`]), saying so on stderr. Returns the last error when
/// every attempt fails.
///
/// This covers the request up to the response's headers. A body that
/// stalls or is cut off part way still fails the install.
pub(crate) fn retry<T>(
    url: &str,
    call: impl FnMut() -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    retry_after(url, &RETRY_DELAYS, call)
}

fn retry_after<T>(
    url: &str,
    delays: &[Duration],
    mut call: impl FnMut() -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    let mut delays = delays.iter();
    loop {
        match call() {
            Err(e) if is_transient(&e) => {
                let Some(delay) = delays.next() else {
                    return Err(e);
                };
                eprintln!("{url}: {e}; trying again in {}s", delay.as_secs());
                std::thread::sleep(*delay);
            }
            r => return r,
        }
    }
}

/// The lowercase host of an `https://` URL.
pub(crate) fn https_host(url: &str) -> Result<String> {
    let uri: ureq::http::Uri = url
        .parse()
        .with_context(|| format!("invalid URL {url:?}"))?;
    if uri.scheme_str() != Some("https") {
        bail!("{url} is not https");
    }
    uri.host()
        .map(str::to_ascii_lowercase)
        .with_context(|| format!("{url} has no host"))
}

/// Fails unless `url` is `https://` on one of `hosts`.
pub fn check_host(url: &str, hosts: &[String]) -> Result<()> {
    let host = https_host(url)?;
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        bail!(
            "{url}: host {host} is not allowed (allowed: {}; GANYMEDE_PYPI_HOSTS adds more)",
            hosts.join(", ")
        );
    }
    Ok(())
}

/// Copies `body` into `out`, failing past `max` bytes or when its SHA-256
/// is not `sha256`.
pub fn save_checked(
    mut body: impl Read,
    out: &mut impl Write,
    max: u64,
    sha256: &str,
) -> Result<()> {
    let mut hash = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = body.read(&mut buf).context("reading")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max {
            bail!("more than the {max} bytes allowed");
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    let got = hex::encode(hash.finalize());
    if !got.eq_ignore_ascii_case(sha256) {
        bail!("sha256 is {got}, expected {sha256}");
    }
    Ok(())
}

/// A wheel to download: its file name, URL, and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wheel {
    pub filename: String,
    pub url: String,
    pub sha256: String,
}

/// The pure wheel (`py3-none-any` or `py2.py3-none-any`) among a release's
/// files, skipping yanked ones: only pure wheels carry the same data files
/// on every platform.
pub fn pick_wheel(files: &Value) -> Option<Wheel> {
    files.as_array()?.iter().find_map(|u| {
        let file = u["filename"].as_str()?;
        if u["packagetype"] != "bdist_wheel" || !is_pure_wheel(file) || u["yanked"] == true {
            return None;
        }
        Some(Wheel {
            filename: file.to_string(),
            url: u["url"].as_str()?.to_string(),
            sha256: u["digests"]["sha256"].as_str()?.to_ascii_lowercase(),
        })
    })
}

/// Whether a wheel's tags (`name-version[-build]-pytag-abi-platform.whl`)
/// make it pure: any platform, no ABI, and a `py3` tag.
pub fn is_pure_wheel(file: &str) -> bool {
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

/// A release's JSON (`/pypi/<project>/<version>/json`) refused when PyPI
/// marks it yanked; the reason, if the uploader gave one, is in the error.
pub fn check_not_yanked(project: &str, meta: &Value) -> Result<()> {
    if meta["info"]["yanked"] == true {
        let version = meta["info"]["version"].as_str().unwrap_or("?");
        match meta["info"]["yanked_reason"]
            .as_str()
            .filter(|r| !r.is_empty())
        {
            Some(r) => bail!("{project} {version} was yanked from PyPI: {r}"),
            None => bail!("{project} {version} was yanked from PyPI"),
        }
    }
    Ok(())
}

/// The newest release in a project's JSON (`/pypi/<project>/json`) that
/// matches `specs` and has a file that is not yanked (PyPI yanks a release
/// by yanking every file). With `pure_only`, the file must be a pure wheel,
/// the only kind [`pick_wheel`] installs; without it, the release is good
/// for reading its metadata. Pre-releases count only
/// when a specifier names one or no final release matches, as pip does;
/// the flag says a pre-release was picked.
pub fn newest_matching(
    project_meta: &Value,
    specs: &[Spec],
    pure_only: bool,
) -> Option<(String, bool)> {
    let releases = project_meta["releases"].as_object()?;
    let mut candidates: Vec<(Version, &str)> = releases
        .iter()
        .filter(|(_, files)| {
            if pure_only {
                pick_wheel(files).is_some()
            } else {
                files
                    .as_array()
                    .is_some_and(|f| f.iter().any(|f| f["yanked"] != true))
            }
        })
        .filter_map(|(v, _)| Some((Version::parse(v)?, v.as_str())))
        .filter(|(v, _)| specs.iter().all(|s| s.matches(v)))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let allow_pre = specs.iter().any(|s| s.version.is_pre());
    let finals = candidates
        .iter()
        .rev()
        .find(|(v, _)| allow_pre || !v.is_pre());
    match finals {
        Some((v, s)) => Some((s.to_string(), v.is_pre())),
        None => candidates.last().map(|(_, s)| (s.to_string(), true)),
    }
}

// ----- PEP 440 versions and specifiers ------------------------------------

/// Whether `s` is a version PyPI can have: it parses, and has no local
/// part and nothing but letters, digits, `.`, `!`, `-`, and `_`.
fn is_public_version(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '!' | '-' | '_'))
        && Version::parse(s).is_some()
}

/// A PEP 440 version, enough to order and compare releases: epoch,
/// release numbers, pre-release (`a`, `b`, `rc`), post and dev releases.
/// A local part (`+...`) must be well formed, and is then ignored.
#[derive(Debug, Clone)]
pub struct Version {
    epoch: u64,
    release: Vec<u64>,
    pre: Option<(u8, u64)>,
    post: Option<u64>,
    dev: Option<u64>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.strip_prefix('v').unwrap_or(&s);
        let s = match s.split_once('+') {
            // A local part is letters and digits, separated by `.`, `-`, or `_`.
            Some((public, local)) => {
                let ok = local
                    .split(['.', '-', '_'])
                    .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric()));
                if !ok {
                    return None;
                }
                public
            }
            None => s,
        };
        let (epoch, rest) = match s.split_once('!') {
            Some((e, r)) => (e.parse().ok()?, r),
            None => (0, s),
        };
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let (nums, mut tail) = rest.split_at(end);
        let nums = nums.trim_end_matches('.');
        if nums.is_empty() {
            return None;
        }
        let release = nums
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<Vec<u64>>>()?;
        if end < rest.len() && rest[..end].ends_with('.') {
            // `1.0.post1`: the dot belongs to the suffix.
            tail = &rest[end - 1..];
        }
        let mut v = Version {
            epoch,
            release,
            pre: None,
            post: None,
            dev: None,
        };
        while !tail.is_empty() {
            tail = tail.trim_start_matches(['.', '-', '_']);
            let word_end = tail
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(tail.len());
            let (word, after) = tail.split_at(word_end);
            let after = after.trim_start_matches(['.', '-', '_']);
            let num_end = after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len());
            let n: u64 = if num_end == 0 {
                0
            } else {
                after[..num_end].parse().ok()?
            };
            tail = &after[num_end..];
            match word {
                "a" | "alpha" => v.pre = Some((0, n)),
                "b" | "beta" => v.pre = Some((1, n)),
                "rc" | "c" | "pre" | "preview" => v.pre = Some((2, n)),
                "post" | "rev" | "r" => v.post = Some(n),
                "dev" => v.dev = Some(n),
                // `1.0-1` is a post release.
                "" if num_end > 0 => v.post = Some(n),
                _ => return None,
            }
        }
        Some(v)
    }

    pub fn is_pre(&self) -> bool {
        self.pre.is_some() || self.dev.is_some()
    }

    /// The release numbers without trailing zeros, which do not count:
    /// `1.0` is `1`.
    fn trimmed(&self) -> &[u64] {
        let n = self
            .release
            .iter()
            .rposition(|&x| x != 0)
            .map_or(0, |i| i + 1);
        &self.release[..n]
    }

    fn key(&self) -> VersionKey<'_> {
        // A bare dev release (`1.0.dev1`) sorts before its pre-releases.
        let pre = match (self.pre, self.post, self.dev) {
            (Some((k, n)), _, _) => (k as i8, n),
            (None, None, Some(_)) => (-1, 0),
            _ => (i8::MAX, 0),
        };
        let post = self.post.map_or((-1, 0), |n| (0, n));
        let dev = self.dev.map_or((i8::MAX, 0), |n| (0, n));
        (self.epoch, self.trimmed(), pre, post, dev)
    }
}

/// Epoch, release, then pre, post, and dev as (rank, number).
type VersionKey<'a> = (u64, &'a [u64], (i8, u64), (i8, u64), (i8, u64));

impl PartialEq for Version {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Version {}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.key().cmp(&o.key())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
    Compatible,
    Arbitrary,
}

/// One PEP 440 version specifier, such as `>=8.0` or `==4.*`.
#[derive(Debug, Clone)]
pub struct Spec {
    op: Op,
    version: Version,
    text: String,
    /// `==1.2.*` or `!=1.2.*`.
    wildcard: bool,
}

impl Spec {
    /// Comma separated specifiers: `>=8,<9`. Empty text is no constraint.
    pub fn parse_list(text: &str) -> Result<Vec<Spec>> {
        text.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Spec::parse)
            .collect()
    }

    pub fn parse(text: &str) -> Result<Spec> {
        let text = text.trim();
        let ops = [
            ("===", Op::Arbitrary),
            ("~=", Op::Compatible),
            ("==", Op::Eq),
            ("!=", Op::Ne),
            (">=", Op::Ge),
            ("<=", Op::Le),
            (">", Op::Gt),
            ("<", Op::Lt),
        ];
        let Some((op, rest)) = ops
            .iter()
            .find_map(|(p, op)| text.strip_prefix(p).map(|r| (*op, r.trim())))
        else {
            bail!("invalid version specifier {text:?}: expected ==, !=, >=, <=, >, <, or ~=");
        };
        let (v, wildcard) = match rest.strip_suffix(".*") {
            Some(v) if matches!(op, Op::Eq | Op::Ne) => (v, true),
            _ => (rest, false),
        };
        let version =
            Version::parse(v).with_context(|| format!("invalid version {v:?} in {text:?}"))?;
        if op == Op::Compatible && version.release.len() < 2 {
            bail!("{text:?}: ~= needs at least two release numbers");
        }
        Ok(Spec {
            op,
            version,
            text: text.to_string(),
            wildcard,
        })
    }

    /// The exact version an `==` pin names, if this is one.
    pub fn pinned(&self) -> Option<&str> {
        (self.op == Op::Eq && !self.wildcard).then(|| self.text[2..].trim())
    }

    pub fn matches(&self, v: &Version) -> bool {
        let s = &self.version;
        match self.op {
            Op::Eq if self.wildcard => prefix_matches(v, &s.release),
            Op::Ne if self.wildcard => !prefix_matches(v, &s.release),
            Op::Eq => v == s,
            Op::Ne => v != s,
            Op::Ge => v >= s,
            Op::Le => v <= s,
            // `>1.0` excludes 1.0's post releases, `<1.0` its pre-releases.
            Op::Gt => v > s && !(v.post.is_some() && s.post.is_none() && same_release(v, s)),
            Op::Lt => v < s && !(v.is_pre() && !s.is_pre() && same_release(v, s)),
            Op::Compatible => v >= s && prefix_matches(v, &s.release[..s.release.len() - 1]),
            Op::Arbitrary => false,
        }
    }
}

impl std::fmt::Display for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

fn same_release(a: &Version, b: &Version) -> bool {
    a.epoch == b.epoch && a.trimmed() == b.trimmed()
}

/// Whether `v`'s release starts with `prefix`, zero-padded.
fn prefix_matches(v: &Version, prefix: &[u64]) -> bool {
    prefix
        .iter()
        .enumerate()
        .all(|(i, p)| v.release.get(i).copied().unwrap_or(0) == *p)
}

/// A `Requires-Dist` entry: its name and specifiers, or `None` when it is
/// only needed for an extra or cannot be read. `name[extra] (>=1.0)` and
/// `name>=1.0,<2; python_version < "3.10"` both parse.
pub fn requirement(req: &str) -> Option<(String, Vec<Spec>)> {
    let (req, marker) = match req.split_once(';') {
        Some((r, m)) => (r, Some(m)),
        None => (req, None),
    };
    if marker.is_some_and(|m| m.contains("extra")) {
        return None;
    }
    let req = req.trim();
    let end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(req.len());
    let name = &req[..end];
    if name.is_empty() {
        return None;
    }
    let mut rest = req[end..].trim();
    if rest.starts_with('[') {
        rest = rest[rest.find(']')? + 1..].trim();
    }
    let rest = rest.trim_start_matches('(').trim_end_matches(')');
    Some((name.to_string(), Spec::parse_list(rest).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    fn ok(spec: &str, version: &str) -> bool {
        Spec::parse_list(spec)
            .unwrap()
            .iter()
            .all(|s| s.matches(&v(version)))
    }

    #[test]
    fn orders_versions_as_pep_440() {
        let order = [
            "1.0.dev1",
            "1.0a1",
            "1.0a2.dev1",
            "1.0a2",
            "1.0b1",
            "1.0rc1",
            "1.0",
            "1.0.post1",
            "1.1",
            "1.10",
            "2!0.1",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.0"), v("1.0.0"));
        assert_eq!(v("1.0-1"), v("1.0.post1"));
        assert_eq!(v("1.0RC1"), v("1.0c1"));
        assert_eq!(v("1.0+local"), v("1.0"));
        assert!(v("4.7.0a2").is_pre() && !v("4.7.0").is_pre());
        for bad in [
            "",
            "abc",
            "1.x",
            ".1",
            "1.0+",
            "1.0+/../../evil/1.0",
            "1.0+a..b",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn matches_specifiers() {
        assert!(ok("==1.0", "1.0.0"));
        assert!(!ok("==1.0", "1.0.1"));
        assert!(ok("==1.2.*", "1.2.9") && !ok("==1.2.*", "1.3"));
        assert!(ok("!=1.2.*", "1.3") && !ok("!=1.0", "1.0"));
        assert!(ok(">=8.0", "8.0") && ok(">=8.0", "8.1.2") && !ok(">=8.0", "7.9"));
        assert!(ok("<=2", "2.0") && !ok("<=2", "2.0.1"));
        assert!(ok(">1.0", "1.1") && !ok(">1.0", "1.0.post1"));
        assert!(ok("<2.0", "1.9") && !ok("<2.0", "2.0a1"));
        assert!(ok("~=3.0.15", "3.0.16") && !ok("~=3.0.15", "3.1.0") && !ok("~=3.0.15", "3.0.14"));
        assert!(ok("~=2.2", "2.9") && !ok("~=2.2", "3.0"));
        assert!(ok(">=8,<9", "8.5") && !ok(">=8,<9", "9.0"));
        for bad in ["8.0", "~=1", "=>1", ">=x"] {
            assert!(Spec::parse_list(bad).is_err(), "{bad}");
        }
        assert_eq!(Spec::parse("==4.6.4").unwrap().pinned(), Some("4.6.4"));
        assert_eq!(Spec::parse("==4.*").unwrap().pinned(), None);
        assert_eq!(Spec::parse(">=4").unwrap().pinned(), None);
        assert!(Spec::parse("==3.0+/../../evil/1.0").is_err());
    }

    #[test]
    fn only_public_versions_reach_a_url() {
        for good in ["1.0", "4.7.0a2", "1!2.0.post1", "1.0-1", "1.0.dev3"] {
            assert!(is_public_version(good), "{good}");
        }
        for bad in [
            "1.0+local",
            "1.0+/../x",
            "1.0/../x",
            "1.0?x",
            "1.0#x",
            "",
            "../1.0",
        ] {
            assert!(!is_public_version(bad), "{bad}");
        }
        let index = Index::new("https://pypi.org/pypi", "").unwrap();
        let err = index
            .json("jupyterlab-widgets", Some("1.0+local"))
            .unwrap_err();
        assert!(err.to_string().contains("public"), "{err}");
    }

    #[test]
    fn reads_requires_dist() {
        let r = |s: &str| requirement(s).map(|(n, specs)| (n, specs.len()));
        assert_eq!(r("ipywidgets>=8.0.0"), Some(("ipywidgets".into(), 1)));
        assert_eq!(r("traitlets (>=4.3.1,<6)"), Some(("traitlets".into(), 2)));
        assert_eq!(r("foo[bar] >=1"), Some(("foo".into(), 1)));
        assert_eq!(r("psygnal"), Some(("psygnal".into(), 0)));
        assert_eq!(
            r("typing-extensions>=4; python_version < \"3.10\""),
            Some(("typing-extensions".into(), 1))
        );
        assert_eq!(r("pytest; extra == \"test\""), None);
        let (_, specs) = requirement("jupyterlab_widgets~=3.0.15").unwrap();
        assert!(specs[0].matches(&v("3.0.16")) && !specs[0].matches(&v("4.0")));
    }

    fn wheel(name: &str, yanked: bool) -> Value {
        json!({"packagetype": "bdist_wheel", "filename": format!("{name}-py3-none-any.whl"),
               "url": format!("https://files.pythonhosted.org/{name}"),
               "digests": {"sha256": "AB"}, "yanked": yanked})
    }

    #[test]
    fn picks_an_unyanked_pure_wheel() {
        let files = json!([
            {"packagetype": "sdist", "filename": "x-1.0.tar.gz",
             "url": "https://x/s", "digests": {"sha256": "aa"}},
            {"packagetype": "bdist_wheel", "filename": "x-1.0-cp312-cp312-win_amd64.whl",
             "url": "https://x/c", "digests": {"sha256": "cc"}},
            wheel("x-1.0", true),
            wheel("x-1.0-1", false),
        ]);
        let w = pick_wheel(&files).unwrap();
        assert_eq!(w.url, "https://files.pythonhosted.org/x-1.0-1");
        assert_eq!(w.sha256, "ab");
        assert_eq!(pick_wheel(&json!([wheel("x-1.0", true)])), None);
        assert_eq!(pick_wheel(&json!([])), None);
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
    fn resolves_the_newest_matching_unyanked_release() {
        let meta = json!({"releases": {
            "1.0": [wheel("x-1.0", false)],
            "1.1": [wheel("x-1.1", false)],
            "1.2": [wheel("x-1.2", true)],
            "2.0b1": [wheel("x-2.0b1", false)],
            "0.9": [],
            "1.3": [
                {"packagetype": "sdist", "filename": "x-1.3.tar.gz", "yanked": false},
                {"packagetype": "bdist_wheel", "filename": "x-1.3-cp312-cp312-win_amd64.whl",
                 "url": "https://x/c", "digests": {"sha256": "cc"}, "yanked": false},
            ],
        }});
        let pick = |s: &str| newest_matching(&meta, &Spec::parse_list(s).unwrap(), true);
        assert_eq!(
            pick(""),
            Some(("1.1".into(), false)),
            "1.2 is yanked, 1.3 has no pure wheel"
        );
        assert_eq!(pick("<=1.3"), Some(("1.1".into(), false)));
        // Read for its requirements, a release needs no pure wheel.
        let any = |s: &str| newest_matching(&meta, &Spec::parse_list(s).unwrap(), false);
        assert_eq!(any(""), Some(("1.3".into(), false)));
        assert_eq!(any("==1.2"), None, "yanked");
        assert_eq!(pick("<1.1"), Some(("1.0".into(), false)));
        assert_eq!(pick(">=2.0b1"), Some(("2.0b1".into(), true)));
        assert_eq!(
            pick(">=1.5"),
            Some(("2.0b1".into(), true)),
            "only a pre-release"
        );
        assert_eq!(pick("==1.2"), None, "yanked even when pinned");
        assert_eq!(pick("==0.9"), None, "no files");
    }

    #[test]
    fn refuses_a_yanked_release() {
        let meta = json!({"info": {"version": "1.2", "yanked": true, "yanked_reason": "broken"}});
        let err = check_not_yanked("x", &meta).unwrap_err().to_string();
        assert!(
            err.contains("x 1.2 was yanked") && err.contains("broken"),
            "{err}"
        );
        assert!(check_not_yanked("x", &json!({"info": {"yanked": false}})).is_ok());
    }

    #[test]
    fn only_https_on_allowed_hosts() {
        let hosts = vec!["files.pythonhosted.org".to_string()];
        assert!(check_host("https://files.pythonhosted.org/p/x.whl", &hosts).is_ok());
        assert!(check_host("https://FILES.pythonhosted.org:443/p/x.whl", &hosts).is_ok());
        for bad in [
            "http://files.pythonhosted.org/p/x.whl",
            "https://evil.example/p/x.whl",
            "https://files.pythonhosted.org.evil.example/x.whl",
            "https://user@evil.example/x.whl",
            "file:///etc/passwd",
        ] {
            assert!(check_host(bad, &hosts).is_err(), "{bad}");
        }
        // A redirect is checked by its final URL, which these cover.
        let idx = Index::new("https://mirror.example/pypi/", "cdn.example, ").unwrap();
        assert_eq!(idx.api, "https://mirror.example/pypi");
        assert_eq!(idx.api_host, "mirror.example");
        assert_eq!(
            idx.file_hosts,
            ["files.pythonhosted.org", "mirror.example", "cdn.example"]
        );
        assert!(Index::new("http://mirror.example/pypi", "").is_err());
    }

    /// A local HTTP server that answers each connection in turn with the
    /// next of `responses`: a status to send, or `None` to read the
    /// request and never answer. It stops listening after the last, so a
    /// request too many is refused. Returns its URL and the number of
    /// requests it has had.
    fn serve(
        responses: Vec<Option<&'static str>>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let mut head = Vec::new();
                let mut buf = [0; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                seen.fetch_add(1, SeqCst);
                match response {
                    Some(status) => {
                        let _ = write!(
                            conn,
                            "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        );
                    }
                    // Left open, so the client waits out its timeout.
                    None => std::mem::forget(conn),
                }
            }
        });
        (url, requests)
    }

    /// [`agent`] without HTTPS only (the local server is plain HTTP) or a
    /// proxy from the environment, waiting `recv_response` for an answer.
    fn plain_agent(recv_response: Duration) -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .proxy(None)
                .timeout_recv_response(Some(recv_response))
                .build(),
        )
    }

    const NO_WAIT: [Duration; 3] = [Duration::ZERO; 3];

    fn requests(n: &std::sync::atomic::AtomicUsize) -> usize {
        n.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn retries_a_request_the_server_fails() {
        let (url, n) = serve(vec![
            Some("504 Gateway Timeout"),
            Some("503 Service Unavailable"),
            Some("200 OK"),
        ]);
        let agent = plain_agent(Duration::from_secs(30));
        let resp = retry_after(&url, &NO_WAIT, || agent.get(&url).call()).unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(requests(&n), 3);
    }

    #[test]
    fn retries_a_request_that_times_out() {
        let (url, n) = serve(vec![None, Some("200 OK")]);
        let agent = plain_agent(Duration::from_secs(1));
        let resp = retry_after(&url, &NO_WAIT, || agent.get(&url).call()).unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(requests(&n), 2);
    }

    #[test]
    fn gives_up_after_the_last_retry() {
        let (url, n) = serve(vec![Some("502 Bad Gateway"); 5]);
        let agent = plain_agent(Duration::from_secs(30));
        let err = retry_after(&url, &NO_WAIT, || agent.get(&url).call()).unwrap_err();
        assert!(matches!(err, ureq::Error::StatusCode(502)), "{err}");
        // The first attempt and one per delay.
        assert_eq!(requests(&n), 1 + NO_WAIT.len());
    }

    #[test]
    fn does_not_retry_what_would_fail_again() {
        // Not found: one request, though the next would have succeeded.
        let (url, n) = serve(vec![Some("404 Not Found"), Some("200 OK")]);
        let agent = plain_agent(Duration::from_secs(30));
        let err = retry_after(&url, &NO_WAIT, || agent.get(&url).call()).unwrap_err();
        assert!(matches!(err, ureq::Error::StatusCode(404)), "{err}");
        assert_eq!(requests(&n), 1);

        // Nothing listening: refused, and not retried.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{port}/file");
        let mut calls = 0;
        let err = retry_after(&url, &NO_WAIT, || {
            calls += 1;
            agent.get(&url).call()
        })
        .unwrap_err();
        assert!(!is_transient(&err), "{err}");
        assert_eq!(calls, 1);

        // A host that can't be reached, or a name that doesn't resolve
        // in time: the next attempt would wait as long again.
        for timeout in [ureq::Timeout::Connect, ureq::Timeout::Resolve] {
            assert!(!is_transient(&ureq::Error::Timeout(timeout)), "{timeout}");
        }
    }

    #[test]
    fn downloads_are_capped_and_hashed() {
        let body = b"wheel bytes".to_vec();
        let sha = hex::encode(Sha256::digest(&body));
        let mut out = Vec::new();
        save_checked(&body[..], &mut out, 100, &sha.to_uppercase()).unwrap();
        assert_eq!(out, body);

        let err = save_checked(&body[..], &mut Vec::new(), 100, "00").unwrap_err();
        assert!(err.to_string().contains("expected 00"), "{err}");

        let err = save_checked(&body[..], &mut Vec::new(), 4, &sha).unwrap_err();
        assert!(err.to_string().contains("4 bytes allowed"), "{err}");
    }
}
