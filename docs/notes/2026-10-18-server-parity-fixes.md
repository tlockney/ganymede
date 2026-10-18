## 2026-10-18, small auth and REST parity fixes (#54)

A group of low-severity gaps against `jupyter_server`, found in the
project review:

- The token was compared with `==` in three places (API auth, page-URL
  token stripping, the login form). All now use the same constant-time
  compare as the `_xsrf` check.
- The login cookie never had `Secure`. It does now when the request came
  over HTTPS, which for this plain-HTTP server means a TLS-terminating
  proxy. `X-Forwarded-Proto`/`X-Scheme` are believed only with the new
  `trust_xheaders` config key, as tornado's `xheaders`, since any client
  can send them.
- `TraceLayer` logged full URIs at debug, `?token=` included. The span now
  records the URI with the token replaced by `[secret]` (`_scrub_uri`).
- Error bodies named absolute OS paths (`/tmp/.../root/link/secret.txt is
  outside the contents root`, config write failures). They name the API
  path or section; the OS path goes to the server log.
- `allow_origins` changed the origin check but sent no CORS headers, and
  a preflight got 403. A `CorsLayer` for the listed origins now echoes the
  origin and answers preflights before auth, with the methods and headers
  `jupyter_server`'s API handlers allow.
- 201 responses (kernels, sessions, contents create and PUT, checkpoints)
  carry `Location`, percent-encoded like `url_escape`.
- Legacy text frames include `"buffers": []`; binary frames still carry
  the buffers outside the JSON.
- `/api/me?permissions=` answers with the requested actions (the token
  grants all of them) and 400s on a malformed value.

Left: `jupyter_server` answers 404, not 403, for paths outside the root.
That is the contents crate's `Forbidden` contract for escapes, shared with
read-only and root-protection errors, so it needs its own variant;
`/api/spec.yaml` is still 404.
