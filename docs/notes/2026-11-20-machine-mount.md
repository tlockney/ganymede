## 2026-11-20, mount a connected machine (#894)

The server forwards an allowlist of routes below `/machines/{id}/` to
another Ganymede server, HTTP and websocket, so an app's page, which only
talks to its own origin, can use a connected machine's files, rooms and
kernels (design: ganymede-ui `docs/design/connected-machines.md`, "How the
app reaches a machine" and "Who may use the mount"). Nothing is mounted
unless the app adds a machine.

Where things are:

- `ganymede-gateway/src/mount.rs`, free of axum: `Machines` (the registry,
  changed at run time, shared with the app through `Startup::machines`),
  `route` (path rules and allowlist), `sanitize_request`/`sanitize_query`/
  `sanitize_response`, `Machine::forward` (reqwest, the same TLS setup as
  `Gateway`, redirects off) and `Machine::connect` (tokio-tungstenite).
- `ganymede-server/src/mount/proxy.rs`: the route `/machines/{id}/{*rest}`
  (merged in `build_router`, behind the `gateway` feature), the files and
  nbconvert policy, and the frame pump between the page's axum socket and
  the machine's. Without `gateway`, `Machines` is a stub whose `add` fails.
- `auth::mount`: who may use it (below).
- `ganymede-ffi`: `ganymede_server_add_machine` and
  `ganymede_server_remove_machine`, in `ganymede.h` and `docs/embedding.md`.

Decisions:

- **Who.** `auth::mount` refuses (403) unless auth is on and the request
  a loopback peer (`ConnectInfo`; none known counts as not loopback; a
  login cookie is no exception, since any token request is given one)
  holds this server's token or its login cookie, then hands over to
  `api_with(.., tailnet: false)`, so cookie requests get the REST API's
  `Origin` and XSRF checks and `identify` never returns `Via::Tailnet`.
  The MCP token is not `state.token`, so it identifies as nothing.
  `strip_token` keeps `/machines/` like `/api/` (a redirect would drop the
  token from a client without a cookie jar).
- **Paths.** `route` reads the raw path after the id and decodes each
  segment alone, so `%2f` and `%5c` are refused instead of becoming
  separators, as are `..` and `.` in any spelling, `\`, NUL, an empty
  segment, and a bad or non-UTF-8 escape (400). The raw path is forwarded.
  `ApiPath::parse` (`ganymede-contents`) is `pub(crate)` and parses one
  decoded path, so it can't see an encoded slash; the machine still checks
  the contents path itself.
- **Stripped** before forwarding: `Cookie`, `Authorization`, `Origin`,
  `Referer`, `X-XSRFToken`/`X-CSRFToken`, `Ganymede-Client`, `Host`,
  hop-by-hop headers (and those `Connection` names), `Sec-Fetch-*`
  (a machine takes a request with `Sec-Fetch-Site` for a browser's, not a
  program's), `Sec-WebSocket-*`, `Forwarded`, `X-Forwarded-*`, `X-Real-IP`,
  `X-Scheme`, `Tailscale-*`, and the `token` and `_xsrf` query parameters
  (other pairs keep their bytes). Then `Authorization: token <t>` when the
  machine has one, and always `Ganymede-Client: ganymede-mount`.
- **Responses.** Status and body (streamed, not rewritten) pass, with only
  an allowlist of headers (`pass_response_header`: content type, length,
  disposition, encoding, range, caching, `X-Content-Type-Options`,
  `Location` rewritten), so `Set-Cookie` never passes. Every response that
  is not a websocket upgrade gets the server's sandbox policy in place of
  the machine's: `PAGE_CSP` for `nbconvert/html/...`, `FILES_CSP` (both
  `pub(crate)` in `routes.rs`) for the rest, on every route; the server's
  own `frame-ancestors` is added to it as for its own `/files`.
- **Websockets.** The machine's socket opens first, offering the page's
  subprotocols, so a machine that is down is a 502 at the handshake and
  the page's socket is accepted with the subprotocol the machine chose.
  Text, binary and close frames pass both ways (a close code that must not
  go on the wire is left off); either side ending closes the other, and
  removing or replacing the machine closes both ends with 1001.
- **Errors.** Unreachable machine: 502 `{"message": ...}` without the URL
  or token; unknown id: 404, after auth, so ids can't be probed without a
  session.

Deviations from the brief, and why:

- `terminals/websocket/{name}` is allowed too: that is where the server
  serves the terminal socket (not under `api/terminals`), so terminals
  would not work through the mount without it.
- `api/nbconvert` (the list of export formats, which JupyterLab asks for
  before `nbconvert/...`) is allowed beside `nbconvert`.
- The sandbox policy is on every forwarded response, not just `files`
  (coordinator's review): `PAGE_CSP` for `nbconvert/html`, `FILES_CSP`
  otherwise.
- Response headers are an allowlist, not a denylist (same review).
- `Location` headers are rewritten: an allowlisted path on the machine gets the
  `/machines/{id}` prefix, a whole URL is dropped. Redirects are not
  followed by the mount, since the page follows them below the prefix.
- `X-Content-Type-Options` is passed from the machine, not forced.
- With auth off the mount is refused, since there is no page session to
  tell from anyone else.

Dependencies and unsafe (for the review):

- `bytes` is a normal dependency of `ganymede-gateway`: already a
  workspace dependency (and in the lockfile), needed for the streamed body
  type reqwest's `chunk()` returns and `Machine::forward` takes.
- `axum` is a dev-dependency of `ganymede-gateway` only, for the stand-in
  machine in `tests/mount.rs` that records what it is sent.
- tokio's `time` feature is added to `ganymede-gateway` for the response
  and connect timeouts.
- Two new `unsafe extern "C"` functions, `ganymede_server_add_machine` and
  `ganymede_server_remove_machine`, follow the crate's C ABI pattern
  (a `# Safety` section on each; the server, `id`, `url` and `token`
  pointers are the caller's, read once and copied). No other `unsafe`.
- No new network surface beyond the mount itself: outbound requests to
  machines the app adds, and the `/machines/{id}/` route (auth above).

Review fixes (second round): a forwarded `Location` is kept only when its
path passes `route` (a machine could otherwise send `/../..` or its
`%2e` forms out of the mount, onto this server's API, and a followed 307
repeats the method, body and credentials); the base-URL prefix is covered
end to end; a machine that accepts but never answers is a 504 after 30
seconds (`RESPONSE_TIMEOUT`, `Machines::with_response_timeout` for tests);
the frame pump runs each direction as its own future so a blocked send
can't stop the other direction being read; OPTIONS and TRACE are 405.

Not covered, for follow-ups (tracked in
[#899](https://github.com/tlockney/ganymede/issues/899)): a published page (`nb publish`) served by
`files` loses its `allow-downloads` (the sandbox is the plain one);
`kernelspecs/{name}/{file}` (logos) isn't on the allowlist; a request in
flight is not cancelled when its machine is removed (websockets are);
`Machines` isn't listed by any API, so the app keeps its own list.

Tests (`ganymede-gateway/tests/mount.rs`, against a second in-process
server, or a stand-in that records what it was sent): refusal for a tailnet
identity, the MCP token, no credentials and auth off; cookie use with
XSRF and `Origin`; the allowlist and traversal (sent raw); credentials
stripped and the machine's added; `Set-Cookie` dropped; the sandbox policy on every route (a machine page
with no policy of its own and a login cookie: `every_response_gets_the_sandbox_policy_and_no_cookie`); add, remove and replace at run time; a machine that is
down (HTTP and websocket); two machines; removal closing sockets; either
side closing; contents GET/PUT/POST/PATCH/DELETE, `files` with a range;
the rooms socket; kernel channels with the subprotocol; the terminal
socket. Unit tests in `mount.rs` for paths and headers;
`the_mount_takes_the_token_and_cookie_only_from_loopback` in `ganymede-server`;
`mounts_and_unmounts_a_machine` and `tests/smoke.c` for the C ABI.

Follow-up (after #900 and ganymede-ui#703 merged): the roadmap says the
journal view is on ganymede-ui's `main` and connected machines are still
being built, as the review of #900 asked.
