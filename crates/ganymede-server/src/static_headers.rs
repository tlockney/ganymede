//! Extra response headers for files served from a generic `static_dir`
//! (`static_headers` in the config), such as the `Cross-Origin-Opener-Policy`
//! and `Cross-Origin-Embedder-Policy` a page needs for `SharedArrayBuffer`.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use std::collections::BTreeMap;

/// Headers for the static paths one pattern matches.
#[derive(Debug, Clone)]
pub struct StaticHeaders {
    /// A path under the base URL, such as `/kernel.html`; one ending in `*`
    /// matches every path that starts with what comes before it.
    pattern: String,
    headers: HeaderMap,
}

impl StaticHeaders {
    /// The rules from the config's `static_headers`, keyed by pattern.
    /// Fails on a pattern that doesn't start with `/` or a header name or
    /// value that isn't valid HTTP.
    pub fn parse(
        config: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> anyhow::Result<Vec<StaticHeaders>> {
        config
            .iter()
            .map(|(pattern, headers)| {
                anyhow::ensure!(
                    pattern.starts_with('/'),
                    "static_headers: path {pattern:?} must start with /"
                );
                let headers = headers
                    .iter()
                    .map(|(name, value)| {
                        let name = HeaderName::try_from(name.as_str()).map_err(|e| {
                            anyhow::anyhow!("static_headers {pattern:?}: header {name:?}: {e}")
                        })?;
                        let value = HeaderValue::try_from(value.as_str()).map_err(|e| {
                            anyhow::anyhow!("static_headers {pattern:?}: {name} value: {e}")
                        })?;
                        Ok((name, value))
                    })
                    .collect::<anyhow::Result<HeaderMap>>()?;
                Ok(StaticHeaders {
                    pattern: pattern.clone(),
                    headers,
                })
            })
            .collect()
    }

    fn matches(&self, path: &str) -> bool {
        match self.pattern.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == self.pattern,
        }
    }
}

/// The request path as the file service resolves it: percent-decoded,
/// without empty or `.` segments, and with `index.html` appended to a
/// directory, so `/kernel%2Ehtml`, `//kernel.html` and `/k/` match the
/// rules for `/kernel.html` and `/k/index.html`.
fn file_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    // As `ServeDir` does: empty and `.` segments don't count, so `//k` and
    // `/./k` are `/k`.
    let decoded = String::from_utf8_lossy(&out);
    let mut path: String = decoded
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .flat_map(|seg| ["/", seg])
        .collect();
    if decoded.ends_with('/') || path.is_empty() {
        path.push_str("/index.html");
    }
    path
}

/// Adds the headers of every rule that matches the request's path, raw or
/// as the file service resolves it, to the response. A header the file
/// service set is replaced; with several rules setting one name, the last
/// in key order wins.
pub async fn add(
    State(rules): State<std::sync::Arc<[StaticHeaders]>>,
    req: Request,
    next: Next,
) -> Response {
    let raw = req.uri().path();
    let paths = [raw.to_owned(), file_path(raw)];
    let mut res = next.run(req).await;
    for rule in rules.iter().filter(|r| paths.iter().any(|p| r.matches(p))) {
        for (name, value) in &rule.headers {
            res.headers_mut().insert(name.clone(), value.clone());
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(pairs: &[(&str, &str, &str)]) -> anyhow::Result<Vec<StaticHeaders>> {
        let mut config: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for (path, name, value) in pairs {
            config
                .entry((*path).into())
                .or_default()
                .insert((*name).into(), (*value).into());
        }
        StaticHeaders::parse(&config)
    }

    #[test]
    fn exact_paths_and_prefixes() {
        let r = rules(&[("/kernel.html", "a", "1"), ("/assets/*", "b", "2")]).unwrap();
        let hits = |p: &str| r.iter().filter(|x| x.matches(p)).count();
        assert_eq!(hits("/kernel.html"), 1);
        assert_eq!(hits("/kernel.html.map"), 0);
        assert_eq!(hits("/assets/a.js"), 1);
        assert_eq!(hits("/assets"), 0);
        assert_eq!(hits("/index.html"), 0);
    }

    #[test]
    fn paths_are_matched_as_the_file_service_resolves_them() {
        assert_eq!(file_path("/kernel%2Ehtml"), "/kernel.html");
        assert_eq!(file_path("/k/"), "/k/index.html");
        assert_eq!(file_path("/"), "/index.html");
        assert_eq!(file_path("/a%2"), "/a%2");
        assert_eq!(file_path("/a%zz"), "/a%zz");
        assert_eq!(file_path("//kernel.html"), "/kernel.html");
        assert_eq!(file_path("/./k//a.js"), "/k/a.js");
        assert_eq!(file_path("/k/./"), "/k/index.html");
    }

    #[test]
    fn invalid_entries_are_errors() {
        assert!(rules(&[("kernel.html", "a", "1")]).is_err());
        assert!(rules(&[("/k", "bad name", "1")]).is_err());
        assert!(rules(&[("/k", "a", "line\nbreak")]).is_err());
    }
}
