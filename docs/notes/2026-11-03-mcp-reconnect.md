## 2026-11-03, `ganymede mcp` reconnects (#297)

When the server ends the proxy's MCP session (a 404), the proxy starts a
new one with the client's `initialize`. It used to clear the session id
before sending that `initialize`, which caused two bugs:

- If the new `initialize` failed (server briefly down, a 5xx), the id
  stayed empty. Every later message went without a session id, got a
  non-404 error, and never retried, so the proxy stayed broken until
  restarted.
- A call sent during the reconnect read the empty id and went without a
  session, so it failed instead of waiting.

Now the stale id stays in place until the new session is ready, and an
`initialize` (the proxy's or the client's own) goes without session
headers. The new id is kept local until `notifications/initialized` has
gone with it, so a half-started session is never published. Calls sent
meanwhile get a 404 and wait on the reconnect lock, then see the new id
and retry. If the start fails, a `failed_starts` count tells the calls
that were waiting to fail too, instead of each sending its own
`initialize` in turn; the next call's 404 tries again.

Tests: `tests/cli.rs` has a small scripted MCP server (`FakeMcp`) that
forgets its sessions, refuses or slows `initialize`, and answers 400 to a
call with no session id, so both bugs fail without the fix. A third test
checks that calls waiting on a failed start fail together after one try.
The test against a restarted real server stays.

Not changed: retries after a reconnect are not ordered against each other
the way `Ticket` orders first sends. Calls that raced into a reconnect
are retried in whatever order the lock releases them. The proxy's HTTP
agent still has no timeout (a cell may run for a long time), so a server
that accepts an `initialize` and never answers holds the reconnect.
