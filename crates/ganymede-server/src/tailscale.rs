//! Tailnet identity as a way in besides the token (#816).
//!
//! Two ways to learn who sent a request, both off by default:
//!
//! - `tailscale serve` proxies a tailnet's HTTPS requests to a port on
//!   loopback and adds who sent them: `Tailscale-User-Login` holds the
//!   sender's login name. A request from a loopback peer whose header
//!   names an allowed login is let in. The header is only as trustworthy
//!   as the loopback port: any process on this machine can connect and
//!   send it. On a machine other people log in to, keep the token.
//! - With `[tailscale] whois` on, a request from a tailnet address
//!   (100.64.0.0/10, fd7a:115c:a1e0::/48), with the server listening on
//!   the tailnet, is looked up with `tailscale whois`: tailscaled knows
//!   which node owns the address, since WireGuard authenticates it. A
//!   tagged node is known by its tags (`tag:agent`), any other by its
//!   owner's login.
//!
//! With `[tailscale] allow` empty both are off; there is no value that
//! allows every tailnet user.

use axum::extract::ConnectInfo;
use axum::http::{Extensions, HeaderMap};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The header `tailscale serve` sets to the sender's login name.
pub const USER_LOGIN: &str = "tailscale-user-login";

/// The `[tailscale]` table of the config file.
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TailscaleConfig {
    /// Tailnet login names (`name@example.com`, `name@github`), and with
    /// `whois` on tags (`tag:agent`), let in without the token. Empty: off.
    pub allow: Vec<String>,
    /// Identify requests from tailnet addresses with `tailscale whois`,
    /// for a server listening on its tailnet address. Off by default.
    pub whois: bool,
    /// The `tailscale` command for `whois`; `tailscale` on `PATH` when
    /// unset. On a Mac with the App Store app, it is
    /// `/Applications/Tailscale.app/Contents/MacOS/Tailscale`.
    pub cli: Option<PathBuf>,
}

impl TailscaleConfig {
    /// Refuses entries that could never match, or would match more than
    /// meant: anything but a login name with an `@`, or a tag. Tags
    /// (`tag:name`) need the sender looked up with `whois`, since `tailscale
    /// serve`'s headers name no one for a tagged device; without `whois`
    /// they are refused.
    pub fn validate(&self) -> Result<(), String> {
        for entry in &self.allow {
            if let Some(name) = entry.strip_prefix("tag:") {
                if !self.whois {
                    return Err(format!(
                        "[tailscale] allow: {entry:?}: tags need `whois = true`; \
                         `tailscale serve` sends no identity for tagged devices"
                    ));
                }
                // Tailscale's rule: a letter, then letters, digits and dashes.
                let ok = name.starts_with(|c: char| c.is_ascii_alphabetic())
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                if !ok {
                    return Err(format!("[tailscale] allow: {entry:?} is not a tag name"));
                }
                continue;
            }
            // `tailscale serve` MIME-encodes non-ASCII values, so such an
            // entry would never match.
            let ok = !entry.is_empty()
                && entry.is_ascii()
                && entry.contains('@')
                && !entry.starts_with('@')
                && !entry.ends_with('@')
                && !entry
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == '*');
            if !ok {
                return Err(format!(
                    "[tailscale] allow: {entry:?} is not an ASCII tailnet login name such \
                     as name@example.com"
                ));
            }
        }
        Ok(())
    }
}

/// Whether the request comes from an allowed login: `allow` is not empty,
/// the connection's peer is a loopback address, and the request has
/// exactly one `Tailscale-User-Login` header naming a login in `allow`.
/// Logins must match exactly, case included.
pub fn allowed_login(allow: &[String], headers: &HeaderMap, ext: &Extensions) -> bool {
    if allow.is_empty() {
        return false;
    }
    // Without the peer address (a router driven without a listener) the
    // header can't be believed.
    let Some(ConnectInfo(peer)) = ext.get::<ConnectInfo<SocketAddr>>() else {
        return false;
    };
    if !peer.ip().to_canonical().is_loopback() {
        return false;
    }
    let mut values = headers.get_all(USER_LOGIN).iter();
    let Some(login) = values.next().and_then(|v| v.to_str().ok()).map(str::trim) else {
        return false;
    };
    if values.next().is_some() || login.is_empty() {
        return false;
    }
    allow.iter().any(|a| a == login)
}

/// Whether `ip` is in Tailscale's address ranges: 100.64.0.0/10 and
/// fd7a:115c:a1e0::/48. Only these are looked up with `whois`.
pub fn is_tailnet_addr(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// Who `tailscale whois` says owns a peer address: a tagged node by its
/// tags only (its login is a placeholder), any other by its owner's login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    Login(String),
    Tags(Vec<String>),
}

impl Peer {
    /// Whether an entry of `allow` names this peer, exactly.
    pub fn allowed(&self, allow: &[String]) -> bool {
        match self {
            Peer::Login(l) => allow.iter().any(|a| a == l),
            Peer::Tags(tags) => tags.iter().any(|t| allow.contains(t)),
        }
    }
}

/// Reads `tailscale whois --json`'s answer. `None` when it names no node,
/// or an untagged node with no login.
pub fn parse_whois(json: &[u8]) -> Option<Peer> {
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "Node")]
        node: Option<Node>,
        #[serde(rename = "UserProfile")]
        user: Option<User>,
    }
    #[derive(Deserialize)]
    struct Node {
        #[serde(rename = "Tags", default)]
        tags: Option<Vec<String>>,
    }
    #[derive(Deserialize)]
    struct User {
        #[serde(rename = "LoginName", default)]
        login: String,
    }
    let r: Resp = serde_json::from_slice(json).ok()?;
    let node = r.node?;
    let tags: Vec<String> = node
        .tags
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.starts_with("tag:"))
        .collect();
    if !tags.is_empty() {
        return Some(Peer::Tags(tags));
    }
    let login = r.user?.login;
    (!login.is_empty() && login.contains('@')).then_some(Peer::Login(login))
}

/// How long an answer from `tailscale whois` is reused: a node that leaves
/// the tailnet, or changes tags, is noticed within this.
const ANSWER_TTL: Duration = Duration::from_secs(60);
/// How long a failed lookup is reused before asking again.
const FAILURE_TTL: Duration = Duration::from_secs(10);
/// How long `tailscale whois` may take.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Addresses remembered at most; the cache starts over past this.
const CACHE_MAX: usize = 4096;

/// Looks up peer addresses with `tailscale whois --json`, caching the
/// answers. One lookup runs at a time; cached answers don't wait for it.
pub struct WhoIs {
    cli: PathBuf,
    /// The server's own tailnet address, which is never looked up: a
    /// process on this machine connecting to it comes from it, and
    /// tailscaled would name this machine's owner.
    own: IpAddr,
    cache: std::sync::Mutex<HashMap<IpAddr, (Instant, Option<Peer>)>>,
    asking: tokio::sync::Mutex<()>,
}

impl WhoIs {
    /// `own` is the tailnet address the server listens on.
    pub fn new(cli: Option<PathBuf>, own: IpAddr) -> Self {
        WhoIs {
            cli: cli.unwrap_or_else(|| "tailscale".into()),
            own: own.to_canonical(),
            cache: Default::default(),
            asking: Default::default(),
        }
    }

    fn cached(&self, ip: IpAddr) -> Option<Option<Peer>> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let (at, peer) = cache.get(&ip)?;
        let ttl = if peer.is_some() {
            ANSWER_TTL
        } else {
            FAILURE_TTL
        };
        (at.elapsed() < ttl).then(|| peer.clone())
    }

    /// The owner of `ip`, a tailnet address other than the server's own;
    /// `None` when tailscaled can't say, or for any other address.
    pub async fn lookup(&self, ip: IpAddr) -> Option<Peer> {
        let ip = ip.to_canonical();
        if !is_tailnet_addr(ip) || ip == self.own {
            return None;
        }
        if let Some(peer) = self.cached(ip) {
            return peer;
        }
        let _asking = self.asking.lock().await;
        // Another request may have asked while this one waited.
        if let Some(peer) = self.cached(ip) {
            return peer;
        }
        let peer = self.ask(ip).await;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(ip, (Instant::now(), peer.clone()));
        peer
    }

    async fn ask(&self, ip: IpAddr) -> Option<Peer> {
        let mut cmd = tokio::process::Command::new(&self.cli);
        cmd.args(["whois", "--json", &ip.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = match tokio::time::timeout(TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                tracing::warn!("tailscale whois: can't run {}: {e}", self.cli.display());
                return None;
            }
            Err(_) => {
                tracing::warn!("tailscale whois {ip}: no answer in {}s", TIMEOUT.as_secs());
                return None;
            }
        };
        if !out.status.success() {
            tracing::debug!("tailscale whois {ip}: {}", out.status);
            return None;
        }
        parse_whois(&out.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(allow: &[&str]) -> TailscaleConfig {
        TailscaleConfig {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn ext(peer: &str) -> Extensions {
        let mut e = Extensions::new();
        e.insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        e
    }

    fn headers(logins: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for l in logins {
            h.append(USER_LOGIN, l.parse().unwrap());
        }
        h
    }

    #[test]
    fn validates_entries() {
        assert!(cfg(&[]).validate().is_ok());
        assert!(cfg(&["me@example.com", "me@github"]).validate().is_ok());
        for bad in [
            "",
            "me",
            "@example.com",
            "me@",
            "*@example.com",
            "a b@c",
            "tag:agent",
        ] {
            assert!(cfg(&[bad]).validate().is_err(), "{bad:?}");
        }
        let whois = |allow: &[&str]| TailscaleConfig {
            whois: true,
            ..cfg(allow)
        };
        assert!(whois(&["tag:agent", "tag:ci-2", "me@github"])
            .validate()
            .is_ok());
        for bad in ["tag:", "tag:2x", "tag:a b", "tag:*", "tag:a_b"] {
            assert!(whois(&[bad]).validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn knows_tailnet_addresses() {
        for ok in [
            "100.64.0.1",
            "100.127.255.254",
            "fd7a:115c:a1e0::1",
            "::ffff:100.100.1.2",
        ] {
            assert!(is_tailnet_addr(ok.parse().unwrap()), "{ok}");
        }
        for no in [
            "100.63.255.255",
            "100.128.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ] {
            assert!(!is_tailnet_addr(no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn reads_whois_answers() {
        let user = br#"{"Node":{"Name":"laptop.example.ts.net.","Tags":null},
            "UserProfile":{"LoginName":"me@example.com","DisplayName":"Me"},"CapMap":{}}"#;
        assert_eq!(
            parse_whois(user),
            Some(Peer::Login("me@example.com".into()))
        );
        // A tagged node's UserProfile is a placeholder; only its tags count.
        let tagged = br#"{"Node":{"Tags":["tag:agent","tag:ci"]},
            "UserProfile":{"LoginName":"tagged-devices"}}"#;
        assert_eq!(
            parse_whois(tagged),
            Some(Peer::Tags(vec!["tag:agent".into(), "tag:ci".into()]))
        );
        assert_eq!(
            parse_whois(br#"{"Node":{},"UserProfile":{"LoginName":"tagged-devices"}}"#),
            None
        );
        assert_eq!(
            parse_whois(br#"{"UserProfile":{"LoginName":"me@example.com"}}"#),
            None
        );
        assert_eq!(parse_whois(b"not json"), None);

        let allow = vec!["me@example.com".to_string(), "tag:agent".to_string()];
        assert!(Peer::Login("me@example.com".into()).allowed(&allow));
        assert!(!Peer::Login("Me@example.com".into()).allowed(&allow));
        assert!(Peer::Tags(vec!["tag:x".into(), "tag:agent".into()]).allowed(&allow));
        assert!(!Peer::Tags(vec!["tag:x".into()]).allowed(&allow));
    }

    #[test]
    fn allows_a_listed_login_from_loopback() {
        let allow = cfg(&["me@example.com"]).allow;
        let h = headers(&["me@example.com"]);
        assert!(allowed_login(&allow, &h, &ext("127.0.0.1:5000")));
        assert!(allowed_login(&allow, &h, &ext("[::1]:5000")));
        assert!(allowed_login(&allow, &h, &ext("[::ffff:127.0.0.1]:5000")));
        // Exactly: an identity provider may tell `Me@` from `me@`.
        assert!(!allowed_login(
            &allow,
            &headers(&["Me@example.com"]),
            &ext("127.0.0.1:5000")
        ));
    }

    #[test]
    fn refuses_everything_else() {
        let allow = cfg(&["me@example.com"]).allow;
        let me = headers(&["me@example.com"]);
        // Off.
        assert!(!allowed_login(&[], &me, &ext("127.0.0.1:1")));
        // Not listed.
        assert!(!allowed_login(
            &allow,
            &headers(&["you@example.com"]),
            &ext("127.0.0.1:1")
        ));
        // Not from loopback, or peer unknown.
        assert!(!allowed_login(&allow, &me, &ext("100.64.0.2:1")));
        assert!(!allowed_login(&allow, &me, &Extensions::new()));
        // No header, an empty one, or two.
        assert!(!allowed_login(&allow, &headers(&[]), &ext("127.0.0.1:1")));
        assert!(!allowed_login(
            &allow,
            &headers(&[" "]),
            &ext("127.0.0.1:1")
        ));
        let two = headers(&["me@example.com", "you@example.com"]);
        assert!(!allowed_login(&allow, &two, &ext("127.0.0.1:1")));
    }
}
