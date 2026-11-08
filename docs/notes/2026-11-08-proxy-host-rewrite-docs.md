## 2026-11-08, a Host-rewriting proxy needs allow_origins (#546)

`docs/security.md` said a proxy that rewrites `Host` to
`127.0.0.1:<port>` needs no setting. It does get past the `Host` check,
but the origin check (`routes.rs` `origin_allowed`, used by the XSRF check
and every websocket) compares the browser's `Origin` with `Host`, and
those no longer match. So saves and kernel connections fail until the
public origin is in `allow_origins`. `jupyter_server` behaves the same way.
The docs now prefer passing `Host` on, and say what the rewrite needs.
