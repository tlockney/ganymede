## 2026-11-14, Tailnet identity behind tailscale serve (#816)

First step of making Ganymede Tailscale-aware. `[tailscale] allow` in
the config lists tailnet login names; a request with no token or login
cookie gets in when it comes from a loopback peer and its
`Tailscale-User-Login` header, which `tailscale serve` sets, names one of
them. Empty by default, so nothing changes unless it is set.

- The server now serves with `into_make_service_with_connect_info`, so
  auth can see the peer address. A request without one (a router driven
  directly, as in tests) never matches.
- The identity is ambient like the login cookie, so it gets the
  cookie's checks: XSRF on changes, `Origin` on the rest. No login
  cookie is set; each request carries the header.
- `/mcp` doesn't take it: agents keep to `mcp_token` or the server's
  token.
- Tags are refused in the list: `tailscale serve` sends no identity for
  tagged devices. They come with the next step, which asks tailscaled
  (WhoIs) for the sender of a server listening on its tailnet address.
- The header is forgeable by any local process, which
  `docs/security.md` states: the option is for a machine one person
  uses.

Design note and the remaining steps are on #816.
