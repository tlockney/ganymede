## 2026-11-03, Bounded awaits in the server e2e tests (#275)

`crates/ganymede-cli/tests/server.rs` bounded each step of `crash()`
with `within`, but the other tests awaited HTTP calls and websocket
connects with no limit, so a stall waited out the job's 15-minute
timeout with no message.

The bounds now live in the shared helpers, so every test gets them:
`Client::call` and `Client::raw` fail after 50 s naming the method and
path, `ws_connect` fails after 20 s naming the step, and `exchange`
bounds its sends. `wait_ready` gives up after 50 s, retrying an attempt
that hangs after 5 s. Each is shorter than the `within` that `crash()`
puts around the same call, so the inner, more specific message is the
one a stall shows.

`crash()` waiting for `restarting` treated any frame other than text,
such as a Ping, as a closed socket. It now skips pings, pongs and
binary frames.
