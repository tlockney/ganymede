//! Authentication as `jupyter_server` does it: the token in an
//! `Authorization: token` header or a `?token=` parameter, or a signed
//! login cookie set when a token-authenticated request arrives without one,
//! plus the `/login` and `/logout` pages and the `_xsrf` check for
//! cookie-authenticated requests that change state.
//!
//! Cookies use tornado's formats, so their shape matches what browsers get
//! from `jupyter_server`: the login cookie is a version 2 signed value
//! (`2|1:0|10:<time>|<name>|<base64 value>|<hex HMAC-SHA256>`) and `_xsrf`
//! is `2|<mask>|<masked token>|<time>`.

use crate::routes::{origin_allowed, HttpError};
use crate::AppState;
use axum::body::Body;
use axum::extract::{OriginalUri, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Form, Router};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::path::Path;

/// Cookies set here expire after 30 days and signed values are accepted
/// for 31, tornado's defaults.
const COOKIE_DAYS: i64 = 30;
const MAX_AGE_DAYS: i64 = 31;

// ----- cookie secret ----------------------------------------------------------

/// The key cookies are signed with. Read from `path` (`jupyter_cookie_secret`
/// in the runtime dir by default, the file `jupyter_server` uses), or
/// generated and written there with owner-only permissions, so a restart
/// keeps browsers logged in. The signing key is derived from the file the
/// way `jupyter_server` derives it with no password configured. If the
/// file cannot be written, the key lives for this process only.
pub fn load_cookie_secret(path: &Path) -> Vec<u8> {
    let key = match std::fs::read(path) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw).expect("system random source");
            let mut key = base64::engine::general_purpose::STANDARD
                .encode(raw)
                .into_bytes();
            key.push(b'\n');
            match write_private(path, &key) {
                Ok(()) => tracing::info!("wrote cookie secret to {}", path.display()),
                Err(e) => tracing::warn!(
                    "cannot write cookie secret to {}: {e}; logins will not survive a restart",
                    path.display()
                ),
            }
            key
        }
    };
    hmac_sha256(&key, b"").to_vec()
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    std::io::Write::write_all(&mut opts.open(path)?, bytes)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ----- signed values ----------------------------------------------------------

fn field(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// Tornado's `create_signed_value`, version 2.
fn sign(secret: &[u8], name: &str, value: &str, at: i64) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(value);
    let to_sign = format!(
        "2|{}|{}|{}|{}|",
        field("0"),
        field(&at.to_string()),
        field(name),
        field(&encoded)
    );
    let sig = hex::encode(hmac_sha256(secret, to_sign.as_bytes()));
    to_sign + &sig
}

/// Tornado's `decode_signed_value` for version 2: the value, if the
/// signature, name, and age check out.
fn verify(secret: &[u8], name: &str, signed: &str, at: i64) -> Option<String> {
    let rest = signed.strip_prefix("2|")?;
    let mut fields = Vec::with_capacity(4);
    let mut rest = rest;
    for _ in 0..4 {
        let (len, tail) = rest.split_once(':')?;
        let len: usize = len.parse().ok()?;
        let value = tail.get(..len)?;
        rest = tail.get(len..)?.strip_prefix('|')?;
        fields.push(value);
    }
    let to_sign = &signed[..signed.len() - rest.len()];
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).ok()?;
    mac.update(to_sign.as_bytes());
    mac.verify_slice(&hex::decode(rest).ok()?).ok()?;
    let [_key_version, timestamp, cookie_name, value] = fields[..] else {
        return None;
    };
    let timestamp: i64 = timestamp.parse().ok()?;
    if cookie_name != name || timestamp < at - MAX_AGE_DAYS * 86400 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    String::from_utf8(bytes).ok()
}

// ----- cookies ------------------------------------------------------------------

/// `username-<host>` with every other character a dash, as
/// `jupyter_server` names it, so servers on different ports of one host
/// keep separate logins.
pub fn login_cookie_name(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    format!("username-{host}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A cookie from the request's `Cookie` headers, unquoted.
pub fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.trim_matches('"').to_string())
        })
}

fn expires(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

fn header_value(s: String) -> HeaderValue {
    HeaderValue::from_str(&s).expect("cookie is visible ASCII")
}

/// The login cookie: HttpOnly, SameSite=Lax, scoped to `base_url`.
/// `jupyter_server` sends no SameSite attribute; Lax keeps the cookie off
/// cross-site subrequests, which the XSRF check otherwise has to catch.
fn login_cookie(state: &AppState, headers: &HeaderMap) -> HeaderValue {
    let name = login_cookie_name(headers);
    let user = serde_json::json!({
        "username": uuid::Uuid::new_v4().simple().to_string(),
        "name": "Anonymous",
        "display_name": "Anonymous",
        "initials": "A",
        "color": null,
        "avatar_url": null,
    });
    let value = sign(&state.cookie_secret, &name, &user.to_string(), now());
    header_value(format!(
        "{name}={value}; expires={}; HttpOnly; Path={}; SameSite=Lax",
        expires(COOKIE_DAYS),
        state.base_url
    ))
}

fn clear_cookie(name: &str, path: &str) -> HeaderValue {
    header_value(format!(
        "{name}=\"\"; expires={}; Path={path}",
        expires(-365)
    ))
}

fn has_login_cookie(state: &AppState, headers: &HeaderMap) -> bool {
    let name = login_cookie_name(headers);
    request_cookie(headers, &name)
        .and_then(|c| verify(&state.cookie_secret, &name, &c, now()))
        .is_some()
}

// ----- XSRF ---------------------------------------------------------------------

/// A new `_xsrf` cookie value: 16 random bytes, masked, in tornado's
/// version 2 format.
fn new_xsrf() -> String {
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).expect("system random source");
    let (mask, token) = bytes.split_at(4);
    let masked: Vec<u8> = token
        .iter()
        .zip(mask.iter().cycle())
        .map(|(t, m)| t ^ m)
        .collect();
    format!("2|{}|{}|{}", hex::encode(mask), hex::encode(masked), now())
}

/// The raw token of an `_xsrf` value, as tornado reads it: version 2 is
/// `2|<4-byte mask>|<masked token>|<time>`, other numbered versions are
/// refused, and anything else is a version 1 token, hex or verbatim.
fn decode_xsrf(v: &str) -> Option<Vec<u8>> {
    let numbered = v.split_once('|').is_some_and(|(n, _)| {
        !n.starts_with('0') && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    });
    let token = if let Some(rest) = v.strip_prefix("2|") {
        let [mask, masked, time] = rest.split('|').collect::<Vec<_>>()[..] else {
            return None;
        };
        time.parse::<i64>().ok()?;
        let mask = hex::decode(mask).ok().filter(|m| m.len() == 4)?;
        hex::decode(masked)
            .ok()?
            .iter()
            .zip(mask.iter().cycle())
            .map(|(t, m)| t ^ m)
            .collect()
    } else if numbered {
        return None;
    } else {
        hex::decode(v).unwrap_or_else(|_| v.as_bytes().to_vec())
    };
    (!token.is_empty()).then_some(token)
}

/// A `Set-Cookie` for a new `_xsrf` cookie, when the request carries no
/// usable one.
pub(crate) fn xsrf_cookie(state: &AppState, headers: &HeaderMap) -> Option<HeaderValue> {
    xsrf_for_page(state, headers).1
}

/// The `_xsrf` cookie to put in a page, and a `Set-Cookie` for it when the
/// request did not carry a usable one.
fn xsrf_for_page(state: &AppState, headers: &HeaderMap) -> (String, Option<HeaderValue>) {
    if let Some(v) = request_cookie(headers, "_xsrf").filter(|v| decode_xsrf(v).is_some()) {
        return (v, None);
    }
    let v = new_xsrf();
    let cookie = header_value(format!(
        "_xsrf={v}; expires={}; Path={}",
        expires(COOKIE_DAYS),
        state.base_url
    ));
    (v, Some(cookie))
}

/// Tornado's `check_xsrf_cookie`: the `_xsrf` argument, or the
/// `X-XSRFToken` or `X-CSRFToken` header, must carry the token in the
/// `_xsrf` cookie. A cross-origin request fails first, with 404.
fn check_xsrf(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
    form: Option<&str>,
) -> Result<(), HttpError> {
    if !origin_allowed(state, headers) {
        return Err(HttpError(404, "Not Found".into()));
    }
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    let sent = form
        .map(String::from)
        .or_else(|| query_param(query, "_xsrf"))
        .or_else(|| header("x-xsrftoken"))
        .or_else(|| header("x-csrftoken"))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| HttpError(403, "'_xsrf' argument missing from POST".into()))?;
    let sent = decode_xsrf(&sent)
        .ok_or_else(|| HttpError(403, "'_xsrf' argument has invalid format".into()))?;
    let expected = request_cookie(headers, "_xsrf").and_then(|c| decode_xsrf(&c));
    let matches = expected.is_some_and(|e| {
        e.len() == sent.len() && e.iter().zip(&sent).fold(0, |acc, (a, b)| acc | (a ^ b)) == 0
    });
    if matches {
        Ok(())
    } else {
        Err(HttpError(
            403,
            "XSRF cookie does not match POST argument".into(),
        ))
    }
}

// ----- identifying a request ------------------------------------------------------

/// How a request authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Token,
    Cookie,
}

fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// `?token=`, else `Authorization: token <t>` (or `bearer`).
fn sent_token(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    query_param(query, "token")
        .filter(|t| !t.is_empty())
        .or_else(|| {
            let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
            let (scheme, rest) = v.split_once(char::is_whitespace)?;
            (scheme.eq_ignore_ascii_case("token") || scheme.eq_ignore_ascii_case("bearer"))
                .then(|| rest.trim().to_string())
        })
}

/// A correct token wins over the cookie, as in `jupyter_server`; `None`
/// when neither authenticates. Only meaningful when auth is on.
pub fn identify(state: &AppState, headers: &HeaderMap, query: Option<&str>) -> Option<Via> {
    let expected = state.token.as_deref()?;
    if sent_token(headers, query).as_deref() == Some(expected) {
        return Some(Via::Token);
    }
    has_login_cookie(state, headers).then_some(Via::Cookie)
}

fn safe_method(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

// ----- middleware ---------------------------------------------------------------

/// For the REST API and websockets: 403 without a token or login cookie.
/// Requests that change state and did not send the token must pass the
/// XSRF check, which `jupyter_server` runs before deciding who the user
/// is. A token-authenticated request without a login cookie gets one.
pub(crate) async fn api(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    if state.token.is_none() {
        // As in `jupyter_server`, a request that is not token-authenticated
        // still needs the XSRF token to change state, so other sites
        // cannot drive a server with auth off.
        if !safe_method(req.method()) {
            if let Err(e) = check_xsrf(&state, req.headers(), req.uri().query(), None) {
                return e.into_response();
            }
        }
        return next.run(req).await;
    }
    let headers = req.headers();
    let via = identify(&state, headers, req.uri().query());
    if via != Some(Via::Token) && !safe_method(req.method()) {
        if let Err(e) = check_xsrf(&state, headers, req.uri().query(), None) {
            return e.into_response();
        }
    }
    match via {
        None => {
            let name = login_cookie_name(headers);
            let mut resp = HttpError(403, "Forbidden".into()).into_response();
            if request_cookie(headers, &name).is_some() {
                resp.headers_mut()
                    .append(header::SET_COOKIE, clear_cookie(&name, &state.base_url));
            }
            resp
        }
        Some(Via::Cookie) => {
            if !origin_allowed(&state, headers) {
                return HttpError(404, "Not Found".into()).into_response();
            }
            next.run(req).await
        }
        Some(Via::Token) => {
            let set = (!has_login_cookie(&state, headers)).then(|| login_cookie(&state, headers));
            let mut resp = next.run(req).await;
            if let Some(c) = set {
                resp.headers_mut().append(header::SET_COOKIE, c);
            }
            resp
        }
    }
}

/// For every route: 403 when `Host` is not a loopback address or a name in
/// `local_hostnames`, unless remote access is allowed. This is
/// `jupyter_server`'s `check_host`, which stops DNS rebinding: a page whose
/// name resolves to 127.0.0.1 is same-origin with itself, so only the
/// `Host` header shows the request is not meant for this server.
pub(crate) async fn check_host(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if host_allowed(&state, req.headers()) {
        next.run(req).await
    } else {
        HttpError(403, "Forbidden".into()).into_response()
    }
}

fn host_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    if state.allow_remote_access {
        return true;
    }
    let Some(host) = headers.get(header::HOST) else {
        return true;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    let name = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((name, "")) => name,
            Some((name, port)) if port.strip_prefix(':').is_some_and(is_port) => name,
            _ => return false,
        },
        None => match host.rsplit_once(':') {
            Some((name, port)) if is_port(port) => name,
            _ => host,
        },
    };
    match name.parse::<std::net::IpAddr>() {
        Ok(addr) => addr.is_loopback(),
        Err(_) => state
            .local_hostnames
            .iter()
            .any(|h| h.eq_ignore_ascii_case(name)),
    }
}

fn is_port(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// For HTML pages: unauthenticated GETs go to the login page with a
/// `next` back here, and anything else gets 403.
pub(crate) async fn page(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if state.token.is_none() || identify(&state, req.headers(), req.uri().query()).is_some() {
        return next.run(req).await;
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return HttpError(403, "Forbidden".into()).into_response();
    }
    let uri = original_uri(&req);
    let next_url: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("next", &uri)
        .finish();
    redirect(&format!("{}login?{next_url}", state.base_url))
}

/// A page URL with a correct `?token=` sets the login cookie and redirects
/// to the same URL without the token, so it does not stay in the address
/// bar or history. API paths and websocket upgrades keep the parameter.
pub(crate) async fn strip_token(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_deref() else {
        return next.run(req).await;
    };
    let path = req.uri().path();
    let api = path == "/api" || path.starts_with("/api/") || path.starts_with("/lab/api/");
    if api
        || !matches!(*req.method(), Method::GET | Method::HEAD)
        || req.headers().contains_key(header::UPGRADE)
        || query_param(req.uri().query(), "token").as_deref() != Some(expected)
    {
        return next.run(req).await;
    }
    let uri = original_uri(&req);
    let (path, query) = uri.split_once('?').unwrap_or((&uri, ""));
    let kept: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| k != "token")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let location = if kept.is_empty() {
        path.to_string()
    } else {
        let q = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(kept)
            .finish();
        format!("{path}?{q}")
    };
    let mut resp = redirect(&location);
    if !has_login_cookie(&state, req.headers()) {
        resp.headers_mut()
            .append(header::SET_COOKIE, login_cookie(&state, req.headers()));
    }
    resp
}

/// The request's path and query before any `base_url` nesting stripped it.
fn original_uri(req: &Request<Body>) -> String {
    let uri = req
        .extensions()
        .get::<OriginalUri>()
        .map(|o| &o.0)
        .unwrap_or(req.uri());
    uri.path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into())
}

fn redirect(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, header_value(location.to_string()))],
    )
        .into_response()
}

// ----- login and logout pages ---------------------------------------------------

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", get(login_form).post(login))
        .route("/logout", get(logout))
        .with_state(state)
}

#[derive(Deserialize, Default)]
struct NextQuery {
    next: Option<String>,
}

#[derive(Deserialize, Default)]
struct LoginForm {
    password: Option<String>,
    next: Option<String>,
    _xsrf: Option<String>,
}

/// `next` if it is a path under `base_url`, else `base_url`. Unlike
/// `jupyter_server`, full URLs are never followed, even to allowed origins.
fn safe_next(state: &AppState, next: Option<&str>) -> String {
    let base = &state.base_url;
    match next {
        Some(n)
            if !n.contains(':')
                && !n.contains('\\')
                && !n.starts_with("//")
                && format!("{n}/").starts_with(base.as_str()) =>
        {
            n.to_string()
        }
        Some(n) => {
            tracing::warn!("not allowing login redirect to {n:?}");
            base.clone()
        }
        None => base.clone(),
    }
}

async fn login_form(
    State(state): State<AppState>,
    Query(q): Query<NextQuery>,
    headers: HeaderMap,
) -> Response {
    if state.token.is_none() || identify(&state, &headers, None).is_some() {
        return redirect(&safe_next(&state, q.next.as_deref()));
    }
    render_login(&state, &headers, q.next.as_deref(), StatusCode::OK, None)
}

async fn login(
    State(state): State<AppState>,
    Query(q): Query<NextQuery>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let next = form.next.as_deref().or(q.next.as_deref());
    let Some(token) = state.token.as_deref() else {
        return redirect(&safe_next(&state, next));
    };
    if let Err(e) = check_xsrf(&state, &headers, None, form._xsrf.as_deref()) {
        return (StatusCode::from_u16(e.0).unwrap(), e.1).into_response();
    }
    if form.password.as_deref() != Some(token) {
        return render_login(
            &state,
            &headers,
            next,
            StatusCode::UNAUTHORIZED,
            Some("Invalid credentials"),
        );
    }
    tracing::info!("browser logged in");
    let mut resp = redirect(&safe_next(&state, next));
    resp.headers_mut()
        .append(header::SET_COOKIE, login_cookie(&state, &headers));
    resp
}

fn render_login(
    state: &AppState,
    headers: &HeaderMap,
    next: Option<&str>,
    status: StatusCode,
    error: Option<&str>,
) -> Response {
    let (xsrf, set) = xsrf_for_page(state, headers);
    let next = next.unwrap_or(&state.base_url);
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("next", next)
        .finish();
    let action = format!("{}login?{query}", state.base_url);
    let esc = crate::lab::html_escape;
    let error = error
        .map(|e| format!("<p class=\"error\">{}</p>", esc(e)))
        .unwrap_or_default();
    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Ganymede login</title>\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<style>body{{font-family:system-ui,sans-serif;max-width:24rem;margin:4rem auto;padding:0 1rem}}\
input{{font:inherit;padding:.4rem;margin:.3rem 0;width:100%;box-sizing:border-box}}.error{{color:#b00}}</style>\
</head><body><h1>Ganymede</h1>{error}<form action=\"{}\" method=\"post\">\
<input type=\"hidden\" name=\"_xsrf\" value=\"{}\"/>\
<label for=\"password_input\">Token</label>\
<input type=\"password\" name=\"password\" id=\"password_input\" autofocus>\
<input type=\"submit\" value=\"Log in\"></form>\
<p>The token is in the URL the server printed when it started.</p></body></html>\n",
        esc(&action),
        esc(&xsrf),
    );
    let mut resp = (status, Html(html)).into_response();
    if let Some(c) = set {
        resp.headers_mut().append(header::SET_COOKIE, c);
    }
    resp
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let name = login_cookie_name(&headers);
    let message = if state.token.is_some() {
        "Successfully logged out."
    } else {
        "Cannot log out. Authentication is disabled."
    };
    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Ganymede</title></head>\
<body><p>{message}</p><p><a href=\"{}login\">Log in again</a></p></body></html>\n",
        crate::lab::html_escape(&state.base_url)
    );
    let mut resp = Html(html).into_response();
    resp.headers_mut()
        .append(header::SET_COOKIE, clear_cookie(&name, &state.base_url));
    if let Some(c) = xsrf_cookie(&state, &headers) {
        resp.headers_mut().append(header::SET_COOKIE, c);
    }
    if state.base_url != "/" {
        resp.headers_mut()
            .append(header::SET_COOKIE, clear_cookie(&name, "/"));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_values_round_trip_and_reject_tampering() {
        let s = sign(b"secret", "username-h", "{\"u\":1}", 1_000_000);
        assert!(s.starts_with("2|1:0|7:1000000|10:username-h|"), "{s}");
        assert_eq!(
            verify(b"secret", "username-h", &s, 1_000_000).as_deref(),
            Some("{\"u\":1}")
        );
        assert_eq!(verify(b"other", "username-h", &s, 1_000_000), None);
        assert_eq!(verify(b"secret", "username-x", &s, 1_000_000), None);
        assert_eq!(
            verify(b"secret", "username-h", &s, 1_000_000 + 32 * 86400),
            None
        );
        let tampered = s.replacen("7:1000000", "7:1000001", 1);
        assert_eq!(verify(b"secret", "username-h", &tampered, 1_000_000), None);
    }

    #[test]
    fn signs_and_verifies_like_tornado() {
        // Made by tornado 6.5's `create_signed_value` with the key derived
        // from a secret file of 44 `0`s and a newline, as jupyter_server
        // 2.21 derives it.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("jupyter_cookie_secret");
        std::fs::write(&file, format!("{}\n", "0".repeat(44))).unwrap();
        let secret = load_cookie_secret(&file);
        let theirs = "2|1:0|10:1700000000|10:username-h|24:eyJ1c2VybmFtZSI6ICJ1In0=|7ec2193852bd40061d43f81e9a5e318e863cecbe39869a7a24e20f8173d2123e";
        assert_eq!(
            verify(&secret, "username-h", theirs, 1_700_000_000).as_deref(),
            Some("{\"username\": \"u\"}")
        );
        assert_eq!(
            sign(
                &secret,
                "username-h",
                "{\"username\": \"u\"}",
                1_700_000_000
            ),
            theirs
        );
    }

    #[test]
    fn xsrf_tokens_unmask() {
        let v = new_xsrf();
        let raw = decode_xsrf(&v).unwrap();
        assert_eq!(raw.len(), 16);
        // Re-masked with a different mask, still the same token.
        let mask = [1u8, 2, 3, 4];
        let masked: Vec<u8> = raw
            .iter()
            .zip(mask.iter().cycle())
            .map(|(t, m)| t ^ m)
            .collect();
        let other = format!("2|{}|{}|0", hex::encode(mask), hex::encode(masked));
        assert_eq!(decode_xsrf(&other).unwrap(), raw);
        assert_eq!(decode_xsrf(&hex::encode(&raw)).unwrap(), raw);
        assert_eq!(decode_xsrf("plain").unwrap(), b"plain");
        for bad in [
            "2|00|00|0",
            "2|00000000||0",
            "2|00000000|00|x",
            "3|a|b|c",
            "",
        ] {
            assert_eq!(decode_xsrf(bad), None, "{bad}");
        }
    }

    #[test]
    fn cookie_names_follow_the_host() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "127.0.0.1:8888".parse().unwrap());
        assert_eq!(login_cookie_name(&h), "username-127-0-0-1-8888");
        h.insert(
            header::COOKIE,
            "a=1; username-127-0-0-1-8888=\"2|x\"".parse().unwrap(),
        );
        assert_eq!(
            request_cookie(&h, "username-127-0-0-1-8888").as_deref(),
            Some("2|x")
        );
    }
}
