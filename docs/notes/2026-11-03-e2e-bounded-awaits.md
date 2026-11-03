## 2026-11-03, Bounded awaits in the server e2e tests (#275)

`crates/ganymede-cli/tests/server.rs` bounded each step of `crash()`
with `within`, but the other tests awaited HTTP calls and websocket
connects with no limit, so a stall waited out the job's 15-minute
timeout with no message.

The bounds now live in the shared helpers, so every test gets them:
`Client::call` and `Client::raw` fail after 75 s naming the method and
path, `ws_connect` fails after 20 s naming the step, and `exchange`
bounds its sends. `wait_ready` gives up after 75 s, retrying an attempt
that hangs after 5 s. A call's bound has to sit above the servers' 60 s
kernel startup timeout, or a slow `POST api/kernels` fails as a stall
before the server reports its own outcome; `crash()`'s outer `within`s
went from 60 s to 90 s to stay above it, so the inner, more specific
message is the one a stall shows.

`crash()` waiting for `restarting` treated any frame other than text,
such as a Ping, as a closed socket. It now skips pings, pongs and
binary frames, under one 30 s deadline for the whole wait, so skipped
frames can't hold it open.
