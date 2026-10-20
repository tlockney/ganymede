# Protocol and kernel host

What a Jupyter server has to get right, component by component: the
requirements Ganymede was built against. Most are met; where Ganymede
does something different or doesn't do it yet, the item says so ("Not
yet", "Ganymede"). The [roadmap](roadmap.md) tracks what is planned. For
how the crates are arranged, see [Architecture](architecture.md).

## Protocol crate

- Message envelope: ZMQ multipart frames: `[identities..., "<IDS|MSG>", hmac,
  header, parent_header, metadata, content, buffers...]`.
- HMAC-SHA256 over the four JSON frames, keyed from the connection file. The
  empty-key case (no signing) must be supported.
- Header: `msg_id`, `session`, `username`, `date` (ISO 8601), `msg_type`,
  `version` ("5.5"), optional `subshell_id` (5.5).
- Typed `content`, with unknown fields preserved (`serde_json::Value`
  passthrough) because kernels add extras. Ganymede types the messages it
  reads or builds (execute, complete, inspect, is_complete, kernel_info,
  shutdown, interrupt, the iopub outputs, stdin, comms); the rest (debug,
  history, subshells) pass through as JSON.
- Five channels: shell (DEALER), control (DEALER), iopub (SUB; XPUB on the
  kernel side as of 5.5), stdin (DEALER), heartbeat (REQ).
- Connection file: JSON with ip, transport (`tcp` or `ipc`), five ports, key,
  signature_scheme, kernel_name. Written to the runtime dir as
  `kernel-<uuid>.json`.
- Messages to cover: execute, inspect, complete, history, is_complete,
  comm_info, kernel_info, shutdown, interrupt, debug (DAP), stream,
  display_data, update_display_data, execute_input, execute_result, error,
  status, clear_output, comm_open/msg/close, input_request/reply, and the
  5.5 subshell messages (create/delete/list) plus `iopub_welcome`. Ganymede
  carries `subshell_id` in the header and passes subshell messages
  through; it neither sends nor needs `iopub_welcome` yet.

ZMQ options: the pure-Rust `zeromq` crate (used by `runtimelib` and Deno's
built-in kernel) or `zmq` bindings to libzmq. Pure Rust avoids a C
dependency; libzmq is the reference behaviour. Ganymede uses pure Rust,
behind the `KernelTransport` trait so it can be swapped.

The `jupyter-protocol`, `runtimelib`, and `nbformat` crates from the
runtimed project cover much of this and are maintained (Zed depends on
them). Ganymede implements its own and tests against the Python reference
implementations instead.

## Kernel host

A long-running process that owns the ZMQ sockets for every kernel and
multiplexes clients onto them. Responsibilities:

- Kernelspec discovery: `kernel.json` under the Jupyter data dirs
  (`JUPYTER_PATH`, `~/.local/share/jupyter/kernels`,
  `/usr/share/jupyter/kernels`, macOS `~/Library/Jupyter`, Windows `%APPDATA%`).
  `jupyter_client` also searches `<sys.prefix>/share/jupyter/kernels`, the
  Python environment it runs in. Ganymede has no Python environment, so it
  doesn't; `--kernelspec-root` adds a conda or venv prefix by hand.
  Fields: `argv` with `{connection_file}` substitution, `env`, `display_name`,
  `language`, `interrupt_mode`, `metadata`.
- Port allocation: bind to find free ports, release, pass to kernel. Racy by
  design; `ipc` transport sidesteps it on POSIX.
- Launch: subprocess with env, cwd, process group (POSIX) or job object
  (Windows) so interrupt and kill reach children. Not yet on Windows:
  there is no job object, and the Windows build is untested.
- Startup handshake: send `kernel_info_request` on shell and control
  repeatedly until both reply. This "nudge" exists because PUB/SUB has a
  slow-joiner problem: iopub messages sent before the subscriber connects
  are lost, including the initial `starting` status. Don't skip it.
- Heartbeat: REQ/REP ping on an interval; missing replies mean the kernel
  is hung or dead. Ganymede pings only kernels it attached to (`run
  --existing`, `Engine::connect`); for a kernel it launched, the process
  exiting is the signal.
- Interrupt: `SIGINT` to the process group when `interrupt_mode` is
  `signal`; `interrupt_request` on control when it is `message`. Windows
  has no SIGINT for this; ipykernel uses a named event via an env var.
- Shutdown: `shutdown_request` on control, wait, then SIGTERM, then
  SIGKILL. Remove the connection file.
- Autorestart: poll the process; restart on unexpected exit, bounded
  retries, with a `restarting` status broadcast to clients. Not yet:
  Ganymede reports the kernel `dead` and removes it.
- Multi-client: many websocket clients per kernel. iopub is broadcast;
  shell replies are routed back to the client whose `session` sent the
  request. `execute_input` on iopub is how other clients see executions.
- Execution semantics the host should not hide: the kernel runs shell
  requests serially; after an error, queued requests come back with
  `status: aborted`; `stop_on_error`, `silent`, `store_history`,
  `allow_stdin`, `user_expressions` are per-request flags.
- Completion detection: a request is done when the shell reply has arrived
  *and* an iopub `status: idle` with the matching `parent_header.msg_id`
  has arrived. The two sockets have no ordering guarantee between them.
- stdin channel: `input_request` from the kernel means a client must answer
  with `input_reply`; only the requesting client should be prompted.

## Server (REST + websocket)

If any existing frontend or tool should connect (JupyterLab, VS Code,
nbclient, papermill, Quarto), the REST surface must match `jupyter_server`:

- `/api/kernelspecs`, `/api/kernels` (list, start, get, delete, interrupt,
  restart), `/api/sessions` (notebook path to kernel mapping),
  `/api/contents` (get, put, patch, delete, checkpoints), `/api/status`,
  `/api/terminals` (optional; Ganymede has none).
- `/api/kernels/{id}/channels` websocket. Two subprotocols: the default
  (JSON with a `channel` key, binary buffers in a separate frame layout)
  and `v1.kernel.websocket.jupyter.org` (single binary frame with offset
  table, channel name, and buffers). Implement v1; the default is for
  older clients.
- Auth: token in header or query, cookie session, XSRF token for
  non-GET, origin check on websocket upgrade, CORS. The server executes
  arbitrary code by design, so this is not optional even on localhost.
- `base_url` prefix support for reverse proxies and JupyterHub.
- Static asset serving for the UI. Ganymede serves JupyterLab's bundle
  from disk, as `ganymede lab install` unpacks it, rather than embedding
  one.

Stack: tokio, axum, tokio-tungstenite, serde, hmac/sha2, uuid, chrono.
There are no terminals, and no file watching yet (the document model on
the [roadmap](roadmap.md)).

## Notebook format

- nbformat 4.5: `cells[]` with `id`, `cell_type` (code, markdown, raw),
  `source` (string or array of lines), `metadata`, `outputs[]`,
  `execution_count`, `attachments` for markdown cells.
- Output types: `stream` (name, text), `display_data`, `execute_result`
  (plus `execution_count`), `error` (ename, evalue, traceback). MIME
  bundles are `{mime: string | string[] | json}`; `application/json` and
  `+json` types are stored as JSON, not strings.
- Round-trip fidelity matters for git diffs: `nbformat` writes
  `indent=1`, `sort_keys=True`, trailing newline. Match it exactly or
  every save produces a diff.
- Validate against the JSON schema; accept v3 and upgrade on load.
  Ganymede reads leniently and reports schema problems the way
  `jupyter_server` does (`message` in the contents model), and upgrades
  v3 notebooks to 4.5 on read.
- `ganymede-nbformat` matches Python's writer byte for byte.

## Document model and collaboration

Jupyter's modern document model is a Yjs document (`jupyter_ydoc`), and
JupyterLab 4 syncs it over `y-websocket`. Rust has `yrs`, the official Yjs
port, and `y-sync` for the protocol. This is the one place where Rust
makes the server simpler than the Python original: one process can own the
CRDT, the file on disk, and the kernel connection.

Retrofitting collaboration onto a non-CRDT model is a rewrite. Ganymede's
plan is the file as the source of truth, reloaded when it changes on disk,
with a `yrs` document as an optional live-session layer later; see
[the iPad use case](ipados.md) for why.

## What is usually missed

The parts a from-scratch server most often gets wrong, and where
Ganymede stands on each.

| Area | Why it matters | Ganymede |
| --- | --- | --- |
| Slow-joiner nudge, and completion as reply plus idle | Without either, the first cell hangs now and then | Both, as above |
| Widgets | Comms are cheap; widgets also need the frontend's widget manager and each package's JavaScript | Comms and binary buffers are routed; `ganymede lab install` adds `jupyterlab_widgets` and [`anywidget`](https://github.com/manzt/anywidget) |
| Binary buffers | Easy to drop in the websocket bridge | Carried in both websocket framings |
| Large and fast output | A busy kernel can flood a slow client | Past 10,000 queued iopub messages a client's output is dropped and it is told, as `jupyter_server`'s rate limit does; replies are never dropped |
| Several clients, reconnects | Laptops sleep and iPads suspend | Kernels outlive connections; a client that reconnects with its session gets what it missed |
| Security | The server runs code; localhost alone doesn't stop DNS rebinding or a malicious notebook | Token, login cookie, XSRF, `Host` and origin checks, notebook trust; see [Security](security.md) |
| Windows | Interrupts, process groups and paths differ | Built, untested; no signal interrupts or job object ([Known gaps](known-gaps.md#platforms)) |
| Debugger (DAP over `debug_request`) | ipykernel advertises it | Passed through on control; not covered by tests yet |
| Subshells (5.5) | New kernels will use them | `subshell_id` and subshell messages passed through |
| Headless execution | Useful alone, and the best conformance test | `ganymede run` and `ganymede execute`, compared against `jupyter execute` in CI |
| Ecosystem contracts | Runtime dir, `jupyter --paths`, `base_url`, JupyterHub | Jupyter's directories and `base_url` match; no JupyterHub single-user mode |
| Extensions | JupyterLab's adoption rests on them | Prebuilt frontend extensions; no server extensions, by design |
| nbformat fidelity | Every save otherwise churns git | Byte-identical to Python's writer |
| Language servers, terminals, export | Each depends on something outside the core | None of them |
