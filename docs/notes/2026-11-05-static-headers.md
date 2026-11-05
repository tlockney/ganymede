## 2026-11-05, extra response headers for static paths (#375)

- `static_headers` in the config (and the ffi's JSON config) maps a path
  under `base_url`, or a prefix ending in `*`, to response headers for
  files from a generic `static_dir`. It exists so ganymede-ui's
  `kernel.html` can be cross-origin isolated (COOP `same-origin`, COEP
  `require-corp`) for Pyodide's `SharedArrayBuffer` (ganymede-ui#258).
  Nothing in the server names a language or a page.
- The headers are added by a layer on the static fallback router only,
  outside `auth::static_page`, so routes the server answers never match;
  an unclaimed path (an unknown `/api/...`) falls through to it and can.
  Rules match the raw path and the path as `ServeDir` resolves it
  (percent-decoded, `index.html` for a directory), so a header can't be
  dodged by spelling. They replace a header of the same name the file service set.
- Patterns are a plain path or a `*` prefix, not globs, to avoid a new
  dependency; both cover the kernel page and an assets directory.
- Bad names or values fail at startup rather than at the first request.
  With JupyterLab as the static dir the option is ignored with a warning;
  JupyterLab has its own routes and doesn't need it.
