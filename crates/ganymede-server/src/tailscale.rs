//! Tailnet identity as a way in besides the token (#816).
//!
//! `tailscale serve` proxies a tailnet's HTTPS requests to a port on
//! loopback and adds who sent them: `Tailscale-User-Login` holds the
//! sender's login name. With `[tailscale] allow` set, a request with no
//! token or login cookie is let in when it comes from a loopback peer and
//! that header names an allowed login. Off by default; there is no value
//! that allows every tailnet user.
//!
//! The header is only as trustworthy as the loopback port: any process on
//! this machine can connect and send it. On a machine other people log in
//! to, keep the token.

use axum::extract::ConnectInfo;
use axum::http::{Extensions, HeaderMap};
use serde::Deserialize;
use std::net::SocketAddr;

/// The header `tailscale serve` sets to the sender's login name.
pub const USER_LOGIN: &str = "tailscale-user-login";

/// The `[tailscale]` table of the config file.
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TailscaleConfig {
    /// Tailnet login names (`name@example.com`, `name@github`) let in
    /// without the token when `tailscale serve` names them. Empty: off.
    pub allow: Vec<String>,
}

impl TailscaleConfig {
    /// Refuses entries that could never match, or would match more than
    /// meant: anything but a login name with an `@`. Tags (`tag:name`)
    /// need the sender looked up with tailscaled's WhoIs, which `tailscale
    /// serve`'s headers don't give; they are refused until then.
    pub fn validate(&self) -> Result<(), String> {
        for entry in &self.allow {
            if entry.starts_with("tag:") {
                return Err(format!(
                    "[tailscale] allow: {entry:?}: tags are not supported yet; \
                     `tailscale serve` sends no identity for tagged devices"
                ));
            }
            let ok = !entry.is_empty()
                && entry.contains('@')
                && !entry.starts_with('@')
                && !entry.ends_with('@')
                && !entry
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == '*');
            if !ok {
                return Err(format!(
                    "[tailscale] allow: {entry:?} is not a tailnet login name such as \
                     name@example.com"
                ));
            }
        }
        Ok(())
    }
}

/// The allowed login a request comes from, when `allow` is not empty, the
/// connection's peer is a loopback address, and the request has exactly
/// one `Tailscale-User-Login` header naming a login in `allow`. Logins are
/// compared without regard to ASCII case.
pub fn allowed_login(allow: &[String], headers: &HeaderMap, ext: &Extensions) -> Option<String> {
    if allow.is_empty() {
        return None;
    }
    // Without the peer address (a router driven without a listener) the
    // header can't be believed.
    let ConnectInfo(peer) = ext.get::<ConnectInfo<SocketAddr>>()?;
    if !peer.ip().to_canonical().is_loopback() {
        return None;
    }
    let mut values = headers.get_all(USER_LOGIN).iter();
    let login = values.next()?.to_str().ok()?.trim();
    if values.next().is_some() || login.is_empty() {
        return None;
    }
    allow
        .iter()
        .find(|a| a.eq_ignore_ascii_case(login))
        .map(|_| login.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(allow: &[&str]) -> TailscaleConfig {
        TailscaleConfig {
            allow: allow.iter().map(|s| s.to_string()).collect(),
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
    }

    #[test]
    fn allows_a_listed_login_from_loopback() {
        let allow = cfg(&["Me@Example.com"]).allow;
        let h = headers(&["me@example.com"]);
        assert_eq!(
            allowed_login(&allow, &h, &ext("127.0.0.1:5000")).as_deref(),
            Some("me@example.com")
        );
        assert!(allowed_login(&allow, &h, &ext("[::1]:5000")).is_some());
        assert!(allowed_login(&allow, &h, &ext("[::ffff:127.0.0.1]:5000")).is_some());
    }

    #[test]
    fn refuses_everything_else() {
        let allow = cfg(&["me@example.com"]).allow;
        let me = headers(&["me@example.com"]);
        // Off.
        assert!(allowed_login(&[], &me, &ext("127.0.0.1:1")).is_none());
        // Not listed.
        assert!(
            allowed_login(&allow, &headers(&["you@example.com"]), &ext("127.0.0.1:1")).is_none()
        );
        // Not from loopback, or peer unknown.
        assert!(allowed_login(&allow, &me, &ext("100.64.0.2:1")).is_none());
        assert!(allowed_login(&allow, &me, &Extensions::new()).is_none());
        // No header, an empty one, or two.
        assert!(allowed_login(&allow, &headers(&[]), &ext("127.0.0.1:1")).is_none());
        assert!(allowed_login(&allow, &headers(&[" "]), &ext("127.0.0.1:1")).is_none());
        let two = headers(&["me@example.com", "you@example.com"]);
        assert!(allowed_login(&allow, &two, &ext("127.0.0.1:1")).is_none());
    }
}
