## 2026-11-27, kernel provider socket (#983)

The socket `docs/kernel-provider.md` proposed: `ganymede serve` binds
`<jupyter_runtime_dir>/ganymede-kernels-<pid>.sock` when
`[kernel_providers] enabled` is on, auth is on and the server isn't
embedded, and names it in `jpserver-<pid>.json` as
`kernel_providers_socket`. A program of the same user connects, sends
`hello` with the server's token, registers display-only kernelspecs and
answers `start`, `send` and `stop` for their kernels. A connection is one
`AppKernel` behind the server's `RegisteredKernels`, so those kernels run
through the same sessions, rooms, execute endpoint and kernel list as any
other, and #984's groundwork (`Origin::Provider`, `started`,
`end_all_from`, `Ended::Gone`) carries the lifetime: on disconnect the
specs are forgotten and the kernels end dead with reason `lost`, never
restarted.

Decisions worth the record:

- **Bound in a private temp dir, then renamed.** A socket's mode is what
  `umask` leaves it, so the socket is bound as `.gk-*/s` inside the
  runtime dir, `chmod 0600`, then `rename`d to its final name: it is
  never visible with a wider mode. `create_private_dir` makes the runtime
  dir `0700` when it's missing, then the server checks it has no group or
  other write bit and is owned by this user, or no socket is opened; in a
  directory another user can write, the socket's own mode would mean
  nothing. The check covers the staged path as well as the final one,
  since on a long runtime dir the staged path is the longer.
- **No `libc` for the uid or `sun_path`.** The staging dir is this
  process's, so its owner is this process's uid, read with
  `MetadataExt::uid`; no `geteuid` and no crate for it. `sun_path`'s size
  is a per-OS constant (104 bytes on macOS and the BSDs, 108 on Linux and
  Android) and a path over one less than that means no socket, with a
  warning naming it. `ganymede-server` gains no `libc` dependency.
- **Auth off means closed.** With `token = ""` anyone who can reach the
  port can already run code, so a provider check adds nothing: the socket
  stays closed with a warning. An embedded server opens none either, and
  Windows serves as usual, warning only when `enabled` is set: the socket
  is Unix only.
- **The handshake and the writes are timed.** `hello` has 10 seconds; a
  write that takes more than 10 seconds closes the connection. `Timing`
  carries the hello deadline, the write stall and the queue size (1024
  frames by default), and a test passes its own to
  `kernel_providers::start` rather than reaching into `Startup`.
- **No `unsafe` in the server, no new dependency.** The socket code has
  no `unsafe`. The new CLI test, `serve_names_the_kernel_provider_socket`,
  adds two `libc::kill(pid, SIGTERM)` calls, as the suite's other `serve`
  tests do, to stop the `ganymede serve` it spawned: the pid is that
  child's, read from its own `Child`, so the signal can only reach the
  process the test started. `base64` was already
  in the workspace: the JSON message helpers move it from `ganymede-ffi`
  to `ganymede-protocol`, so neither gains or loses a crate version.
- **Reservation is #986.** `[kernel_providers] reserve` and the
  code-signature check are not here; names are first come, and a name a
  kernelspec on disk or a kernel server lists is refused.
- A provider's `start` is answered in two steps, since a frame can't return
  a value: `started`, then the kernel's `kernel_info_reply`. A deferred
  start waits up to `startup_timeout_secs` for the first and up to
  `startup_timeout_secs` again for the second. The `start` frame carries
  only `env.JPY_SESSION_NAME`: a provider runs the kernel where and with
  whatever environment it likes, so no `cwd` or other `env` is sent, and
  no `argv`/`env` from a `register` is kept. A close takes the state lock
  before marking the connection closed, and `start` queues under the same
  lock, so a launch racing a disconnect leaves no pending `started`
  sender.

Tests. `crates/ganymede-server/tests/kernel_providers.rs` drives
`kernel_providers::start` and the router with a fake provider over a real
`UnixStream`: off by default (no socket); the handshake (wrong token, no
`hello`, another `op` first, each `welcome` with an error and then a
closed connection); the socket's `0600` mode and its name in the info
file; the happy path (register, listed without `argv`/`env`, start
through `POST /api/kernels`, a cell through the execute endpoint whose
output is the fake's `execute_result`, interrupt on control, `DELETE`
sending `shutdown_request` then `stop`); first-come names and
`unregister`; a dropped connection (kernel gone from `/api/kernels` and
its spec from `/api/kernelspecs`, no restart, `GET /api/kernels/<id>` a
404, a reconnected provider's next start a new id); and a provider that
stops reading (a 16 MiB message past a 200 ms stall closes the
connection, the kernel ends `lost`, `restartable()` false); a full queue
(a four-frame queue past a 60-second stall closes the connection, the
kernel ends `lost`); a start that carries only `env.JPY_SESSION_NAME`,
both from `POST /api/kernels` with no session and from a session created
through `POST /api/sessions` (no `cwd`, no other `env`); a second
connection that can neither deliver into nor end the first's kernel (its
`status: busy` doesn't reach the kernel's execution state, and its
`ended` brings no restart, so the provider sees one `start`); a refused
start (a 500 with the provider's reason, one `start` frame, no kernel
left); a start nobody answers (the API's 500 at the startup timeout, no
kernel left); a silent connection closed at the `hello` deadline; each
refusal leaving no socket (off, embedded, no token, a world-writable
runtime dir, a path over `sun_path`'s limit); and the listener dropped
(a live provider reads EOF, and the socket file is gone). Unit tests in
the module cover `peer_allowed`, the runtime-dir mode and owner check
(`0700` yes, `0770`/`0777`/another owner no), the `sun_path` limit and
the check of each path against it, the
spec field stripping, a `start` after a close (no pending `started`), and
a full queue closing the connection rather than dropping a frame.
`server_info.rs` and the CLI suite cover the JSON key's presence and
absence and the socket file's life.

What the tests can't cover: a connection from another user. CI has one
user, so `peer_allowed`'s refusal is a unit test and the accept path's
use of it is read, not exercised. A path over the limit is also refused
by `bind` itself, so the length check is proved by its own unit test, not
by the integration test that leaves no socket.
