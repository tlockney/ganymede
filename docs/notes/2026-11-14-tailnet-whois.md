## 2026-11-14, Tailnet identity from tailscale whois (#816)

Second step of making Ganymede Tailscale-aware. With `[tailscale] whois
= true`, a server listening on its tailnet address asks `tailscale whois
--json <ip>` who owns the address of each request that has no token or
cookie. Tagged devices are known by their tags, others by their owner's
login, so `allow` may now list tags (`tag:agent`); without `whois` tags
are still refused.

- Only tailnet addresses (100.64.0.0/10, fd7a:115c:a1e0::/48) are looked
  up, one lookup at a time, with a five-second timeout; cached answers
  don't wait for a lookup. Answers are cached for a minute, failures for
  ten seconds.
- The server's own address is never looked up: a local process that
  connects to it comes from it, and whois would name the machine's owner
  (found in review). So `whois` requires `ip` to be the tailnet address
  itself; `0.0.0.0` or another address is refused at startup. Calling the CLI rather
  than tailscaled's LocalAPI covers the Linux daemon and both Mac apps,
  whose LocalAPI sockets differ.
- Browsers send no Fetch Metadata over plain HTTP, so they get no
  identity this way; JupyterLab stays on `tailscale serve`. Programs
  (the apps, the gateway, scripts) send a `Ganymede-Client` header and
  no `Origin`. A page on another site can't send a custom header
  without a CORS preflight, so the header also replaces the XSRF token
  for them. The header works behind `tailscale serve` too. A program's
  websocket handshake may add a same-origin `Origin` of either scheme
  alongside it, since some libraries do and a browser's `WebSocket`
  can't set the header.
- With `[tailscale] allow` set, `allow_remote_access` now defaults to
  off whatever `--ip` is, so the `Host` check stays on, and
  `local_hostnames` accepts IP addresses. With `whois` on, the server
  adds its own `--ip` to them.
- New process surface: the server runs the `tailscale` command (or the
  one in `[tailscale] cli`). It is off by default and only with `allow`
  set.
