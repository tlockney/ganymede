## 2026-11-14, Tailnet identity names the person (#816)

Third step of making Ganymede Tailscale-aware. A request let in by
tailnet identity now carries the name it was let in as, and the server
uses it where it named a person before:

- `/api/me` returns the login (or a tagged device's tags) as the user's
  name and initials, so JupyterLab shows who you are instead of an
  anonymous name. A request with the identity is named even when it
  also carries the login cookie or the token (found in review: a
  browser that opened the token URL once would otherwise stay
  anonymous). Without it, `/api/me` is anonymous as before.
- A shared notebook's room credits edits to that name (`Room::connect_as`),
  whatever name the frontend puts in its awareness state, so a person
  with a tailnet identity can't be credited under another name. Other
  clients still name themselves, and may pick a tailnet login; `"by"`
  doesn't say which names were vouched for. Marking them is a possible
  follow-up.
- The request's log span gets a `who` field (debug level), and each name
  is logged at info level the first time it's seen, once per server.
- With `whois` on, a token or cookie request from a tailnet address is
  looked up too, so it is named; it can then wait on `tailscale whois`
  (cached a minute, at most five seconds). Cross-origin ones, whose
  answer would go unused, aren't looked up.
- `ganymede info` shows which logins the config file lets in
  (`tailnet_identity`, null when off), and the error when `serve` would
  refuse the config (`serve::check_tailscale`, shared with startup).

`/api/me`'s response keeps its shape; only the values change for a
tailnet login. Not yet tried on a real tailnet.
