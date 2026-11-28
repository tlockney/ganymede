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
  `umask` leaves it, so the socket is bound as `.ganymede-kernels-*/socket`
  inside the runtime dir, `chmod 0600`, then `rename`d to its final name:
  it is never visible with a wider mode. The runtime dir must already be
  owner-only (no group or other write bit, owned by this user), or no
  socket is opened; in a directory another user can write, the socket's
  own mode would mean nothing. `create_private_dir` makes it `0700` when
  it's missing.
- **The peer's uid from tokio, no new crate for it.** `UnixStream::peer_cred`
  gives the uid, so the check is `peer_allowed(peer_uid, own_uid)` over
  `libc::geteuid()`. `libc` is only for that and for `sun_path`'s size
  (`sockaddr_un.sun_path.len() - 1`: 103 bytes on macOS, 107 on Linux, a
  longer path means no socket and a warning naming it), and is a
  `[target.'cfg(unix)'.dependencies]` entry.
- **Auth off means closed.** With `token = ""` anyone who can reach the
  port can already run code, so a provider check adds nothing: the socket
  stays closed with a warning. An embedded server opens none either, and
  Windows serves as usual, warning only when `enabled` is set.
- **The handshake and the writes are timed.** `hello` has 10 seconds; a
  write that takes more than 10 seconds closes the connection, and so does
  a full 1024-frame queue: a provider that stops reading is cut off rather
  than written to forever, as the events socket does (#980). `Timing`
  carries both, and `Startup::kernel_provider_timing` lets a test shorten
  them without waiting.
- **Reservation is #986.** `[kernel_providers] reserve` and the
  code-signature check are not here; names are first come, and a name a
  kernelspec on disk or a kernel server lists is refused.
- A provider's `start` is answered in two steps, since a frame can't return
  a value: `started`, then the kernel's `kernel_info_reply`. A deferred
  start waits up to `startup_timeout_secs` for the first and up to
  `startup_timeout_secs` again for the second.

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
connection, the kernel ends `lost`, `restartable()` false). Unit tests in
the module cover `peer_allowed`, the runtime-dir mode check (`0700` yes,
`0770`/`0777`/another owner no), the `sun_path` limit, and the spec
field stripping. `server_info.rs` covers the JSON key's presence and
absence.

What the tests can't cover: a connection from another user. CI has one
user, so `peer_allowed`'s refusal is a unit test and the accept path's
use of it is read, not exercised. The socket's `sun_path` refusal is
likewise a unit test of the limit; the warning path isn't driven.
