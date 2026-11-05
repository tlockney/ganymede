## 2026-11-05, response headers for static paths (#375)

The Pyodide kernel planned for the Apple apps (ganymede-ui#258) needs
`SharedArrayBuffer` for interrupts and synchronous `input()`, so the page
that hosts kernels must be cross-origin isolated (COOP `same-origin`,
COEP `require-corp`). A service worker can't set them reliably in
WKWebView.

`static_headers` in `ganymede.toml` (and the same key in the ffi config
JSON, which deserializes the same `Config`) maps a URL path under a
generic `static_dir` to extra headers. A key is an exact path or a prefix
ending in `*`; no glob crate, since the one use is a single page. The
headers are parsed at start, so a bad name or value stops the server
instead of failing per request. They apply in the `static_page`
middleware that already adds the `_xsrf` cookie, so they reach only
static files, never the API, and not JupyterLab's bundle, which has its
own router. Nothing names a language.
