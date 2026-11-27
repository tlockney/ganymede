# Brief: kernel provider socket (tlockney/ganymede#983)

Branch `claude/project-thread-riqt1l`, from `main`. Read first:
`docs/kernel-provider.md` (the design), issue #983 and its comments (this brief is the scope),
`crates/ganymede-kernel-host/src/registered.rs` (the groundwork from #985:
`Origin::Provider`, `register`, `unregister`, `AppKernel::started`,
`Ended::Gone`, `end_all_from`), `crates/ganymede-ffi/src/lib.rs`
(`impl AppKernel for CKernel`, the C ABI this channel mirrors),
`crates/ganymede-server/src/serve.rs` (around the `registered` set at
~line 911 and the `ServerInfoFiles::write` call at ~line 1115),
`crates/ganymede-server/src/server_info.rs`,
`crates/ganymede-server/src/config.rs`, and the events socket's
backpressure (`/api/events/subscribe`, #980) for the queue pattern.

Out of scope (issue #986): `[kernel_providers] reserve` and any code
signature check. Names are first come only. ganymede-ui changes are out
of scope.

## 1. Config

`[kernel_providers]` table in `Config` (`crates/ganymede-server/src/config.rs`),
a `KernelProvidersConfig { enabled: bool }` with `#[serde(default,
deny_unknown_fields)]`, default `enabled = false`. Document it where the
other config keys are documented (grep docs for `mcp_enabled` or
`[tailscale]` to find the config reference). No CLI flag.

## 2. Listener (new module `crates/ganymede-server/src/kernel_providers.rs`, `#[cfg(unix)]`)

Opened only when all hold: `kernel_providers.enabled`, the server is not
embedded (`!embedded`, same condition as the info files), auth is on (a
non-empty token), and the platform is Unix. Each case that keeps it
closed while `enabled` is on logs one `warn!` saying why.

Socket path: `<jupyter_runtime_dir>/ganymede-kernels-<pid>.sock`.

| Case | Behavior |
| --- | --- |
| runtime dir missing | create with `create_private_dir` (0700) |
| runtime dir exists and is group- or world-writable (`mode & 0o022 != 0`) or not owned by this uid | don't open; warn naming the dir and its mode |
| path longer than the platform's `sun_path` limit (104 bytes on macOS, 108 on Linux; compute from `libc::sockaddr_un` `sun_path` length, minus 1 for NUL) | don't open; warn with the path and its length |
| a stale file at the path (left by an earlier process with this pid) | remove it, then bind |
| bind | bind in a fresh private temp dir inside the runtime dir (`tempfile::Builder::tempdir_in`, mode 0700), `chmod 0600` the socket there, then `rename` it to the final path, so it is never visible with a wider mode. Remove the temp dir. |
| server stops | remove the socket file (a Drop guard, like `ServerInfoFiles`) and close every connection |

Only when the socket is open, add `"kernel_providers_socket": "<absolute path>"`
to `jpserver-<pid>.json` (extend `ServerInfo` with an
`Option<&Path>` field; absent key when None). The listener must be bound
before the info file is written.

`libc` is already a dependency of other crates in the workspace; add
`libc = "0.2"` to ganymede-server under `[target.'cfg(unix)'.dependencies]`
only if you need it for `sun_path`'s size, and say why in the dev log.
Peer credentials: use tokio's `UnixStream::peer_cred()` (uid), no new
crate. Note it in the dev log.

## 3. Connection protocol

Newline-delimited JSON, one object per line, field `op`. Frames exactly
as the tables in docs/kernel-provider.md "Frames". Max line length 64 MiB
(a Jupyter message with buffers can be large); a longer line closes the
connection.

Per connection, in order:

| Step | Rule |
| --- | --- |
| accept | `peer_cred().uid()` must equal the server's effective uid (`libc::geteuid()`); otherwise close at once with no frame. Make the uid check a small pure fn `peer_allowed(peer_uid, own_uid) -> bool` so a unit test covers it. |
| first frame | must be `hello` with `token` equal to the server's token, compared in constant time (the server already has a constant-time compare for tokens; reuse it). Within 10 s. Anything else, or a timeout: send `{"op":"welcome","error":"..."}` and close. |
| `welcome` | sent on success |
| `register` | `spec` keeps only `name`, `display_name`, `language`, `metadata`; everything else (`argv`, `env`, `interrupt_mode`, ...) is dropped before it reaches `RegisteredKernels::register(spec, Origin::Provider, ...)`. Reply `registered {name}` or `registered {name, error}` with the error text from `register`. The connection's `AppKernel` is one `Arc` per connection, reused for every spec it registers. |
| `unregister` | only a name this connection registered; else ignored with a `debug!` (no frame defined). Calls `RegisteredKernels::unregister`. |
| `started` | completes the `oneshot` that `AppKernel::started` returned for that `kernel_id`; `error` present → `Err(error)`. Unknown id: ignore. |
| `deliver` | `RegisteredKernels::deliver(kernel_id, channel, msg)`; only for kernel ids this connection was asked to `start`. Parse `msg` as the websocket JSON form (reuse the existing JSON <-> `Message` conversion the C ABI or websocket code uses; buffers base64). |
| `ended` | `RegisteredKernels::ended(kernel_id, Exited or Lost)` for `reason` `exited`/`lost`; only its own ids. |
| unknown `op` or bad JSON | close the connection (treated as a disconnect below). |

The `AppKernel` impl for a connection (`ProviderKernel`): `start` enqueues
`{"op":"start","kernel_id","launch":{"cwd","env":{"JPY_SESSION_NAME"}}}`
and returns Ok (Err only when the connection is already closed);
`started` returns the oneshot receiver registered for that id; `send`
enqueues `send`; `stop` enqueues `stop`. None of them block.

Outgoing frames go through a bounded `mpsc` (capacity 1024) drained by a
writer task. Queue full (`try_send` fails), or a single write that takes
more than 10 s → close the connection. Never drop a frame silently.

## 4. Disconnect

On close for any reason: `registered.end_all_from(&app, Ended::Gone)`
(ends its kernels dead with reason `lost`, no restart, forgets its specs
and provider runs), fail any pending `started` oneshots, log at `info!`
with the count. A reconnect is a new connection with a new `AppKernel`
and gets new kernels.

## 5. Wiring in `serve.rs`

Start the listener after the `KernelManager` exists (it needs the
`RegisteredKernels` Arc already in `AppState.registered`) and before the
info file is written. Hold its guard for the server's lifetime; drop it
on shutdown. Windows: nothing (cfg out), with no warning unless `enabled`
is set, in which case warn that the channel is Unix-only.

## 6. Tests

Integration tests in `crates/ganymede-server/tests/` (look at how
existing tests start a server with a config and a temp
`JUPYTER_RUNTIME_DIR`), with a fake provider written in the test over a
real `tokio::net::UnixStream`. Cases:

1. `enabled` off (default): no socket file, no `kernel_providers_socket` key.
2. Happy path: hello, welcome, register a spec, it appears in
   `/api/kernelspecs`; start a kernel through `/api/kernels`; the fake
   answers `start` with `started` and then a `kernel_info_reply` on shell
   (and status idle on iopub, whatever the host waits for; mirror what
   the registered.rs `Fake` tests send); run code through the execute
   endpoint and get the fake's `execute_result`; interrupt (an
   `interrupt_request` arrives on control); shutdown (a `stop` frame, or
   `shutdown_request` then `stop`).
3. Bad token → `welcome` with `error`, closed. No `hello` within 10 s →
   closed (use a short timeout constant under `cfg(test)` or
   `tokio::time::pause` if the server runs on the test runtime;
   otherwise make the timeout a config-free constant overridable in tests).
4. A name on disk (put a kernelspec in a temp kernelspec root) →
   `registered` with `error`. A name held by another connection → error.
5. `argv`/`env` in a spec are dropped: register a spec with `argv`, then
   check `/api/kernelspecs` shows no argv and a start goes to the provider.
6. Dropped connection: kernel's `/api/kernels/<id>` shows
   `execution_state: "dead"` with reason `lost` (match how #985's tests
   observe `Ended::Gone`), it is not restarted, the spec is gone from
   `/api/kernelspecs`. Reconnect, register again, start → a new kernel id.
7. Slow provider: a fake that never reads; drive enough `send` frames
   (e.g. many executes or a large message) to fill the queue or hit the
   10 s write stall (shorten the stall under test) → connection closed,
   kernels lost.
8. Unit tests: `peer_allowed`, the runtime-dir mode check
   (0700 ok, 0770/0777 refused), the `sun_path` length check, spec field
   stripping.
9. `server_info.rs`: the JSON includes `kernel_providers_socket` only when given.

Peer of another user can't be arranged in CI: the unit test of
`peer_allowed` covers it; say so in the dev log.

## 7. Docs

- `docs/kernel-provider.md`: status from "proposed" to implemented
  (#983), with the reservation pending in #986. Reword per the review
  points: the runtime dir must already be owner-only or the socket isn't
  opened; the socket is made 0600 before it is visible; `sun_path` limit
  behavior; auth off → no socket; a lost kernel's own restart never goes
  to the fallback, but a later launch of the name goes to whatever lists
  it then (a kernelspec installed on disk afterwards wins). Remove or
  mark as "in #986" the `reserve` bullet.
- The frames reference: add a "Kernel providers" section to
  `docs/embedding.md` (or `docs/api.md` if that fits the existing
  structure better) with both frame tables, the hello/welcome handshake,
  the 10 s hello timeout, the queue (1024 frames) and 10 s stall rule,
  and the timeouts: a deferred start waits up to the kernel startup
  timeout for `started`, then up to the startup timeout again for
  `kernel_info_reply` (so up to twice `startup_timeout_secs`). Note the
  known limit: the server keeps one small entry per kernel id a provider
  started until its connection closes.
- `docs/security.md`: the boundary (off by default, Unix only, owner-only
  runtime dir, 0600 socket, peer uid check, token, display-only specs,
  no shadowing, lost on disconnect, auth off → closed).
- Config reference: `[kernel_providers] enabled`.
- Each new section gets this note (CLAUDE.md), once per section:

```md
> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.
```

- Dev log: `docs/notes/2026-11-27-kernel-provider-socket.md` starting
  `## 2026-11-27, kernel provider socket (#983)`: what was built, the
  decisions above (temp-dir bind and rename, uid check via tokio,
  auth-off refusal, reserve moved to #986, timeouts), what tests cover
  and what they can't (another user). Do not edit docs/notes.md.

## 8. Commands to run, all must pass

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
```

Public docs must not link private items. Commit in logical commits on the
branch; don't push. Don't launch any app, take screenshots, or run
`defaults`.

## Last step

Delete `HERMES_BRIEF.md` in your final commit (`git rm HERMES_BRIEF.md`).
