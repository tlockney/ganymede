## 2026-11-14, Tailnet identity behind tailscale serve (#816)

First step of making Ganymede Tailscale-aware. `[tailscale] allow` in
the config lists tailnet login names; a request with no token or login
cookie gets in when it comes from a loopback peer and its
`Tailscale-User-Login` header, which `tailscale serve` sets, names one of
them. Empty by default, so nothing changes unless it is set.

- The server now serves with `into_make_service_with_connect_info`, so
  auth can see the peer address. A request without one (a router driven
  directly, as in tests) never matches.
- The identity is ambient, more so than the login cookie: `tailscale
  serve` adds it to every request from the device, with or without
  credentials. So it gets XSRF on changes and a stricter `Origin` check
  than the cookie: same origin only, never `allow_origins`. And it needs
  the `Host` check: with `allow_remote_access` on, a DNS-rebinding page
  could come in through serve as the visitor, so the server refuses to
  start with both set. No login cookie is set; each request carries the
  header. Another site's subresource requests (`<script>`, `<img>`)
  carry no `Origin` but do get the identity, so a cross-site
  `Sec-Fetch-Site` is refused too, except for a top-level navigation,
  which the SameSite=Lax cookie also allows (GET and HEAD only). Browsers
  send `Sec-Fetch-Site` only over HTTPS, so a request without it gets no
  identity: the feature is for `tailscale serve`'s HTTPS proxy, and
  sandboxed HTML under `/files/` loses its images for a tailnet login.
  (All found in review.)
- `/mcp` doesn't take it: agents keep to `mcp_token` or the server's
  token.
- Tags are refused in the list: `tailscale serve` sends no identity for
  tagged devices. They come with the next step, which asks tailscaled
  (WhoIs) for the sender of a server listening on its tailnet address.
- The lab page renders an empty token for a tailnet-identified
  request. With the token in the page, JupyterLab would switch to it:
  past the cookie's checks, onto `/mcp`, and kept after the login is
  removed from `allow` (found in review).
- The header is forgeable by anything that reaches the loopback port
  other than `tailscale serve`'s HTTP proxy: a local process, another
  reverse proxy, TCP-mode serve or funnel, an SSH tunnel.
  `docs/security.md` says so; the option is for a machine one person
  uses, behind `tailscale serve` only. A non-loopback `ip` with `allow`
  set logs a warning.
- Entries must be ASCII, since `tailscale serve` MIME-encodes other
  characters.

Design note and the remaining steps are on #816.
