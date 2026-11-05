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

/// Adds the headers of every rule that matches the request's path to the
/// response, replacing any the file service set.
pub async fn add(
    State(rules): State<std::sync::Arc<[StaticHeaders]>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let mut res = next.run(req).await;
    for rule in rules.iter().filter(|r| r.matches(&path)) {
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
    fn invalid_entries_are_errors() {
        assert!(rules(&[("kernel.html", "a", "1")]).is_err());
        assert!(rules(&[("/k", "bad name", "1")]).is_err());
        assert!(rules(&[("/k", "a", "line\nbreak")]).is_err());
    }
}
