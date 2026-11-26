## 2026-11-26, embedded servers take room locks (#966)

`serve.rs` took room locks only when `startup.stop.is_none()`, that is,
only for `ganymede serve`/`lab`/`ui`. Every server started with `stop`
took none: the Mac app's (through ganymede-ffi), and `ganymede mcp`'s own
server (`crates/ganymede-cli/src/mcp.rs` passes `stop` too). So the app and
a `ganymede serve` on one notebook could both edit and autosave the file. The comment blamed iOS, which has no runtime dir to lock in,
but the condition turned locking off on the Mac too.

What changed:

- `Startup::room_locks`, a `RoomLocks` enum (`RuntimeDir`, the default,
  or `Off`) in `serve.rs`: `run_with` locks in the Jupyter runtime dir's
  `ganymede-rooms/` unless the app asks otherwise. Nothing infers from
  `stop` any more; `Startup`'s `Default` still locks.
- `ganymede-ffi`'s `startup` sets it: `Off` on iOS (`cfg!(target_os =
  "ios")`, logged), `RuntimeDir` everywhere else, in both the gateway
  and the no-kernels branch. The app's server runs on a thread with a
  runtime of its own, so when the app stops it the rooms and their
  locks are dropped with the runtime; a process exit drops them for the
  CLI servers.
- The read-only errors name the cause, since it is almost always
  another Ganymede server on this machine: `room_ops::READ_ONLY` (the
  execute endpoint's 400 and the room's own refusal) and the MCP tools'
  error (`mcp.rs`) now say "another Ganymede server on this machine, such as the
  Mac app or a `ganymede serve`". `room_lock.rs`, `collab.rs` and the comment in `run_with`
  say who doesn't lock: iOS only.
- Docs ([../rooms.md](../rooms.md) "Two servers on one notebook", and
  the alt text, `title` and footnote of `images/rooms/two-servers.svg`)
  drop "An app that embeds the server takes no lock" for "Every server
  locks, an embedding app's included; only iOS, which has no runtime
  dir, takes no lock", with the standard Unreleased note.
  [../embedding.md](../embedding.md) gains "Two servers on one notebook"
  (`Startup::room_locks`, and that the app and CLI must see the same
  runtime dir), and [../mcp.md](../mcp.md) says `ganymede mcp`'s own
  server now locks too.

Tests:

- `crates/ganymede-server/tests/embedded.rs`
  `an_embedded_server_locks_its_rooms_first_come`: two servers started
  as ganymede-ffi runs them (`stop` set, no output) on one notebook.
  Whichever opens the room first owns the file (its contents API says
  `writable`, the other's says not); the read-only room undoes its
  client's edit and saves nothing while the owner saves its own; the
  other order too: the first server stops, its lock goes with it, the
  second's room takes the file over within its retry tick (about a
  second; the test leaves room before editing), and a fresh server is
  read-only then.
- The embedded test starts each server on a thread of its own with a
  tokio runtime of its own, as the ffi does. On the test's shared
  runtime the room task outlives `run_with` (it is `tokio::spawn`ed,
  and a room holds its lock until it retires, a minute after its last
  client) and the lock never releases.
- Both test binaries set `JUPYTER_RUNTIME_DIR` to a temp dir once, in a
  `OnceLock` before any server starts, so their rooms lock there and
  not in the real `~/Library/Jupyter/runtime`: the servers run in this
  process, so the var can't be set per server as
  `crates/ganymede-cli/tests` set it per command. Set once and never
  changed, parallel tests can't race it, and each test's notebooks are
  in its own temp root, so their locks never meet. The dir is kept,
  not deleted on drop: the servers' rooms lock in it while the binary
  runs.
- `crates/ganymede-ffi/tests/server.rs`
  `the_apps_server_honours_another_servers_room_lock`: holds a room lock
  as another process's room would and checks the C ABI server's contents
  API reports the notebook as not writable, then writable once it is
  let go. It fails with `RoomLocks::Off` in the ffi's `startup`.
- The gateway tests (`ganymede-gateway` mount and remote,
  `ganymede-server` gateway) pass `RoomLocks::Off`: locking isn't under
  test there, and they would otherwise leave lock files in the real
  runtime dir.

Follow-ups: the Mac app doesn't yet show a notebook another server owns
as read-only (ganymede-ui#839); the by-hand check is on
ganymede-ui#399. Tracking: #965.
