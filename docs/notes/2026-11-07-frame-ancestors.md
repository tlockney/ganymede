## 2026-11-07, frame-ancestors on every response (#429)

No response carried `frame-ancestors` or `X-Frame-Options`, so any page
could frame `/lab` (with `--token ""`), and with a token a page on
another `localhost` port could too, since the `SameSite=Lax` login
cookie counts that as same-site.

`build_router`'s outermost layer now adds `frame-ancestors 'self'` to
each response's `Content-Security-Policy`: the whole policy when there
is none, appended to one a handler set (`/files` sends
`sandbox allow-scripts; frame-ancestors 'self'`, the same directives
as `jupyter_server`'s `FilesHandler`), and left alone when the policy
already names `frame-ancestors`, so `static_headers` can widen it for
a static page.

Review: `static_headers` doesn't reach JupyterLab or the server's own
routes, so a site that framed `/lab` had no way back. The sources are a
config key, `frame_ancestors` (default `'self'`), checked at start for
`;` and `,` so a value can't add other directives. With a `base_url`,
the layer is applied again outside the nest so paths outside the prefix
carry it too; a second pass sees the directive and changes nothing.

The UI's output frames use `srcdoc`, not server URLs, and the apps load
pages top-level, so neither is affected.
