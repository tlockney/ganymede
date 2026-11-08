## 2026-11-08, login form body limit (#536)

`POST /login` reads its form before the XSRF check can run, because
`_xsrf` is one of the form's fields. With only the server-wide
`max_body_size` (512 MiB by default), an unauthenticated client could make
the server buffer that much per request. The route now has its own
`DefaultBodyLimit` of 64 KiB, which is far more than a token, `_xsrf` and
`next` need. A layer on the route overrides the router-wide one, because
the innermost `DefaultBodyLimit` sets the extension last.

A route layer replaces the router-wide limit rather than taking the
smaller one, so the route uses `min(64 KiB, max_body_size)`, and a
`max_body_size` below 64 KiB still caps `/login`
(`login_keeps_a_smaller_max_body_size`).
