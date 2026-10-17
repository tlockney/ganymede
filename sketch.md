# Sketch: a Rust reimplementation of the Jupyter core

Scope as stated: a kernel host, a presentation layer (web UI), and the essential
protocol. This note breaks that into components, estimates each, lists the parts
that are usually missed, and proposes an order of work.

Revised goal (2026-10-17): a Jupyter server with no Python anywhere in
the product. The first target kernel is [cdp-kernel](https://cdp-kernel.org),
a TypeScript/JavaScript kernel on Node or Deno that speaks ZMTP over TCP.
That fixes several things below: ZeroMQ is mandatory, the wire format is
fixed, kernelspec discovery has to match the Jupyter data-dir conventions
cdp-kernel installs into, and every place `jupyter_server` assumes Python
or ipykernel must be replaced rather than ported. ipykernel stays useful as
a conformance oracle during development; it is not a dependency.

## 1. What Jupyter actually is

The reference stack, with rough Python line counts, so the "core" has a size:

| Layer | Reference impl | Approx. size | Rust equivalent |
|---|---|---|---|
| Wire protocol (ZMQ, HMAC, message schema) | `jupyter_client` messaging spec v5.5 | spec only | `jupyter-protocol` crate exists |
| Kernel lifecycle (spec discovery, launch, ports, restart, interrupt, shutdown) | `jupyter_client` KernelManager/Provisioner | ~10k | `runtimelib` covers part |
| Notebook file format | `nbformat` v4.5 + JSON schema | ~3k | `nbformat` crate exists |
| Server (REST: kernels, sessions, contents, kernelspecs; websocket bridge; auth) | `jupyter_server` | ~15k | nothing complete |
| Headless execution | `nbclient` | ~2k | nothing |
| Realtime collaboration | `jupyter_collaboration` (Yjs / pycrdt) | ~5k + JS | `yrs` crate is the Rust Yjs |
| Frontend | JupyterLab (`@jupyterlab/*`, Lumino) | ~300k TS | nothing; Zed REPL is a partial native UI |
| Widgets | `ipywidgets` + `@jupyter-widgets/*` | ~50k TS | nothing |

The last two rows are the bulk of the work and the least Rust-shaped.

## 2. Components

### 2.1 Protocol crate

- Message envelope: ZMQ multipart frames: `[identities..., "<IDS|MSG>", hmac,
  header, parent_header, metadata, content, buffers...]`.
- HMAC-SHA256 over the four JSON frames, keyed from the connection file. The
  empty-key case (no signing) must be supported.
- Header: `msg_id`, `session`, `username`, `date` (ISO 8601), `msg_type`,
  `version` ("5.5"), optional `subshell_id` (5.5).
- Typed `content` for every message type, with unknown fields preserved
  (`serde_json::Value` passthrough) because kernels add extras.
- Five channels: shell (DEALER), control (DEALER), iopub (SUB; XPUB on the
  kernel side as of 5.5), stdin (DEALER), heartbeat (REQ).
- Connection file: JSON with ip, transport (`tcp` or `ipc`), five ports, key,
  signature_scheme, kernel_name. Written to the runtime dir as
  `kernel-<uuid>.json`.
- Messages to cover: execute, inspect, complete, history, is_complete,
  comm_info, kernel_info, shutdown, interrupt, debug (DAP), stream,
  display_data, update_display_data, execute_input, execute_result, error,
  status, clear_output, comm_open/msg/close, input_request/reply, and the
  5.5 subshell messages (create/delete/list) plus `iopub_welcome`.

ZMQ options: the pure-Rust `zeromq` crate (used by `runtimelib` and Deno's
built-in kernel) or `zmq` bindings to libzmq. Pure Rust avoids a C
dependency; libzmq is the reference behaviour. Pick pure Rust, keep the
transport behind a trait so it can be swapped.

The `jupyter-protocol`, `runtimelib`, and `nbformat` crates from the
runtimed project already cover much of this and are maintained (Zed depends
on them). Decide early whether the goal is a product (use them) or
understanding (reimplement, use them as the conformance oracle).

### 2.2 Kernel host

A long-running process that owns the ZMQ sockets for every kernel and
multiplexes clients onto them. Responsibilities:

- Kernelspec discovery: `kernel.json` under the Jupyter data dirs
  (`JUPYTER_PATH`, `~/.local/share/jupyter/kernels`, `<sys.prefix>/share/jupyter/kernels`,
  `/usr/share/jupyter/kernels`, macOS `~/Library/Jupyter`, Windows `%APPDATA%`).
  Fields: `argv` with `{connection_file}` substitution, `env`, `display_name`,
  `language`, `interrupt_mode`, `metadata`.
- Port allocation: bind to find free ports, release, pass to kernel. Racy by
  design; `ipc` transport sidesteps it on POSIX.
- Launch: subprocess with env, cwd, process group (POSIX) or job object
  (Windows) so interrupt and kill reach children.
- Startup handshake: send `kernel_info_request` on shell and control
  repeatedly until both reply. This "nudge" exists because PUB/SUB has a
  slow-joiner problem: iopub messages sent before the subscriber connects
  are lost, including the initial `starting` status. Don't skip it.
- Heartbeat: REQ/REP ping on an interval; missing replies mean the kernel
  is hung or dead.
- Interrupt: `SIGINT` to the process group when `interrupt_mode` is
  `signal`; `interrupt_request` on control when it is `message`. Windows
  has no SIGINT for this; ipykernel uses a named event via an env var.
- Shutdown: `shutdown_request` on control, wait, then SIGTERM, then
  SIGKILL. Remove the connection file.
- Autorestart: poll the process; restart on unexpected exit, bounded
  retries, with a `restarting` status broadcast to clients.
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

### 2.3 Server (REST + websocket)

If any existing frontend or tool should connect (JupyterLab, VS Code,
nbclient, papermill, Quarto), the REST surface must match `jupyter_server`:

- `/api/kernelspecs`, `/api/kernels` (list, start, get, delete, interrupt,
  restart), `/api/sessions` (notebook path to kernel mapping),
  `/api/contents` (get, put, patch, delete, checkpoints), `/api/status`,
  `/api/terminals` (optional).
- `/api/kernels/{id}/channels` websocket. Two subprotocols: the default
  (JSON with a `channel` key, binary buffers in a separate frame layout)
  and `v1.kernel.websocket.jupyter.org` (single binary frame with offset
  table, channel name, and buffers). Implement v1; the default is for
  older clients.
- Auth: token in header or query, cookie session, XSRF token for
  non-GET, origin check on websocket upgrade, CORS. The server executes
  arbitrary code by design, so this is not optional even on localhost.
- `base_url` prefix support for reverse proxies and JupyterHub.
- Static asset serving for the UI (embed with `rust-embed`).

Stack: tokio, axum, tokio-tungstenite, serde, hmac/sha2, uuid, chrono,
portable-pty (terminals), notify (file watching).

### 2.4 Notebook format

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
- Reuse or match the `nbformat` crate.

### 2.5 Document model and collaboration

Jupyter's modern document model is a Yjs document (`jupyter_ydoc`), and
JupyterLab 4 syncs it over `y-websocket`. Rust has `yrs`, the official Yjs
port, and `y-sync` for the protocol. This is the one place where Rust
makes the server simpler than the Python original: one process can own the
CRDT, the file on disk, and the kernel connection.

Decide early whether the document is a CRDT or a plain struct. Retrofitting
collaboration onto a non-CRDT model is a rewrite.

### 2.6 Presentation layer

The UI is the open-ended part, and it is mostly not Rust.

Required pieces:

- Cell list with command and edit modes, keyboard shortcuts, move, split,
  merge, copy, paste, undo.
- Code editor: CodeMirror 6 (JupyterLab default) or Monaco. Language mode
  comes from `kernel_info_reply.language_info.codemirror_mode`.
- Completion and inspection popups wired to `complete_request` and
  `inspect_request`.
- Markdown cells: CommonMark + GFM tables, math via KaTeX or MathJax,
  sanitized HTML, cell attachments.
- Output area with a MIME renderer registry and a priority order:
  `application/vnd.jupyter.widget-view+json`, `text/html`, `image/svg+xml`,
  `image/png`, `image/jpeg`, `text/markdown`, `text/latex`, `text/plain`,
  plus `application/json`, Vega/Vega-Lite, Plotly. `text/html` with
  `<script>` and `isolated` metadata means iframe sandboxing.
- Stream handling: merge consecutive chunks from the same stream, honor
  `\r` (progress bars), `\b`, and ANSI SGR colors.
- `clear_output(wait=True)`, `update_display_data` by `display_id`,
  output truncation for runaway loops, iopub rate limiting.
- Execution indicators: `In [*]`, `In [n]`, busy/idle, queued, aborted.
- `input()` prompt in the cell output area, password mode.
- Kernel picker, status bar, restart/interrupt controls.
- File browser, open/save, autosave, "dirty" tracking, checkpoints.
- Trust model: HTML/JS outputs in a notebook you did not execute are
  untrusted and must be sanitized until re-executed. JupyterLab stores
  trust signatures in a local database.

Technology choice:

1. TypeScript UI, Rust server. Reuse CodeMirror, KaTeX, and optionally
   `@jupyterlab/rendermime` and `@jupyterlab/outputarea` as libraries without
   the rest of JupyterLab. Most pragmatic.
2. Rust WASM UI (Leptos, Dioxus, Yew). Every output renderer, the editor,
   math, and widgets are JS libraries, so the WASM layer becomes an interop
   shell. Costs more than it saves.
3. Native UI (GPUI as Zed does, egui, iced). Loses `text/html` output,
   which breaks pandas, plotly, bokeh, altair, and ipywidgets. Fine for a
   REPL, not for a general notebook.
4. Tauri or similar: option 1 packaged as a desktop app with the Rust core
   in-process, no websocket hop. Reasonable later step.

Recommendation: option 1, with option 4 as a packaging target.

## 3. Where Python leaks into the Jupyter stack, and what replaces it

The messaging protocol, the websocket framing, the REST API, and nbformat
are language-neutral. Python shows up around them. Each item below is a
place the reference stack assumes Python; the right column is what a
Python-free server does instead.

| Python assumption in the reference stack | Replacement |
|---|---|
| `jupyter --paths`, `sys.prefix/share/jupyter` as a kernelspec search root | Search only the platform dirs: `$JUPYTER_PATH`, `$JUPYTER_DATA_DIR/kernels`, `~/Library/Jupyter/kernels` (macOS), `${XDG_DATA_HOME:-~/.local/share}/jupyter/kernels` (Linux), `/usr/local/share/jupyter/kernels`, `/usr/share/jupyter/kernels`. cdp-kernel writes to exactly these. |
| `jupyter kernelspec list/install/remove` is a Python CLI | Ship the same subcommands in the server binary. cdp-kernel's uninstall docs currently point at the Python CLI; this closes that gap. |
| Default kernel name is `python3` (`MappingKernelManager.default_kernel_name`, nbformat defaults, sessions API fallback) | No default. A session request without a kernel name is an error, or the default is a config value with no built-in value. |
| Kernel provisioners are Python classes loaded by entry point | A Rust trait with a local-subprocess implementation. A remote implementation is the natural second one, which matches the ts-on-ipados plan of kernels on the homelab. |
| Windows interrupt via a named event in an env var set by `jupyter_client` for ipykernel | Support `interrupt_mode: signal` and `interrupt_mode: message` only. cdp-kernel interrupts over CDP internally; check its generated `kernel.json` for which mode it declares. Windows is out of scope until cdp-kernel supports it. |
| `ipc` transport for connection files | cdp-kernel uses `node:net` TCP only. Default to `tcp`; keep `ipc` as an option for kernels that support it. |
| Widgets mean ipywidgets: Python model classes plus `@jupyter-widgets/controls` | Widgets mean the comm protocol plus the `jupyter.widget` state-sync protocol plus anywidget's `_esm`/`_css` loading. cdp-kernel is anywidget-only. The frontend needs a widget manager that implements that subset without the ipywidgets controls library. This is much smaller than full ipywidgets support. |
| Trust signatures in `nbsignatures.db` via Python's sqlite and a secret file | Own trust store: HMAC over the notebook with a per-user key, stored in the server's data dir. Same model, no shared file format needed. |
| nbconvert endpoints (`/nbconvert/*`) and export menus | Drop. Export is a separate tool if wanted. |
| Server extensions via Python entry points, traitlets config, `jupyter_server_config.py` | Drop. One config file (TOML) for the server. No extension system in v1. |
| JupyterHub single-user contract | Drop for v1. It is an auth handshake plus env vars and can be added later without touching the core. |
| Terminals via `terminado` | `portable-pty`. |
| Language-specific UI behaviour: `%matplotlib`, `pip` magics, Python tracebacks | None. The only source of language information is `kernel_info_reply.language_info`; the editor mode comes from `codemirror_mode`, which cdp-kernel reports as TypeScript. Traceback rendering is ANSI text, not a Python format. |
| Optional protocol messages treated as required (`history`, `is_complete`, `inspect`) | Treat every message beyond `kernel_info`, `execute`, `shutdown`, `interrupt`, and `status` as optional. Read `supported_features` when present. Time out and degrade rather than hang. |

What cdp-kernel needs from the server, from its docs:

- A connection file and the five TCP ports, HMAC-SHA256 signing, and
  the usual startup nudge.
- The server's environment passed through to the kernel process, plus the
  kernelspec `env` block. cdp-kernel documents that the kernel inherits the
  server's environment.
- Control-channel passthrough for `debug_request` on Node and Deno
  targets. The UI needs a DAP client to use it; JupyterLab's debugger
  extension is the reference.
- stdin channel for `prompt()` and `confirm()`.
- Comm routing with binary buffers for anywidget state.

Frontends that work against a Python-free server today, before any UI is
written:

- VS Code's Jupyter extension connects to a remote server over the REST
  API and websocket. With cdp-kernel this is a fully Python-free setup.
- JupyterLab's built frontend is static JS. Serving it needs the
  `/lab/api/*` endpoints (settings, workspaces, translations, listings)
  and a page config. Jupyverse, a Python ASGI reimplementation of
  `jupyter_server`, documents this endpoint set and is the closest prior
  art for a from-scratch server.

The validation path is therefore: server plus cdp-kernel plus VS Code.
The UI is a later, separate decision.

## 4. What is usually missed

- **Widgets.** Comms are cheap: route `comm_*` messages and buffers. Widgets
  are not: they need a browser-side widget manager, model state sync, and
  loading widget JS shipped with Python packages (via labextension or CDN).
  Users expect ipywidgets, bokeh, plotly, and itables to work. Either
  embed `@jupyter-widgets/html-manager` or declare widgets out of scope.
- **The slow-joiner nudge** and the dual completion condition (shell reply
  plus idle status). Both produce intermittent "notebook hangs on first
  cell" bugs when missing.
- **Windows.** Interrupts, process groups, path handling, ZMQ behaviour.
  Decide whether Windows is a target before designing process management.
- **Security.** Code execution service, HTML output sanitization, trust,
  token auth, XSRF, websocket origin. Localhost-only is not a defense:
  DNS rebinding and malicious notebooks are the standard attack paths.
- **Binary buffers** in messages (widgets, arrays). Easy to drop in the
  websocket bridge.
- **Large and fast output.** Rate limiting iopub, truncating outputs,
  not re-rendering the whole output on each stream chunk.
- **Multiple clients on one kernel** and reconnect after websocket drop
  without losing the kernel.
- **Debugger** over `debug_request` (DAP). Optional, but the protocol
  surface is in 5.5 and ipykernel advertises it.
- **Subshells** (5.5). New kernels will support them; the host should at
  least pass `subshell_id` through.
- **Headless execution** (nbclient, papermill). Cheap once the host
  exists, useful on its own, and the best conformance test.
- **Config and ecosystem contracts.** `jupyter --paths`, runtime dir,
  JupyterHub single-user contract, `base_url`. Skipping these is fine for
  a personal tool; it is the difference between "works for me" and
  deployable.
- **Extension story.** JupyterLab's adoption rests on extensions. A core
  without one is a different product; say so in the goals.
- **Language server integration**, terminals, nbconvert export, and
  `%matplotlib` inline backends all depend on things outside the core.
- **nbformat round-trip fidelity** (indentation, key order) so saves do
  not churn git history.

## 5. Suggested order of work

1. Protocol crate plus connection files. Launch cdp-kernel from a CLI,
   run one cell, print the result. Conformance-test against cdp-kernel's
   own suite where possible, and against ipykernel and evcxr as oracles.
2. nbformat crate (or adopt `nbformat`) with round-trip tests against
   real notebooks.
3. Headless executor: `run notebook.ipynb --out`. This is nbclient and
   gives a usable tool at milestone 3.
4. Server: kernelspecs (with `list/install/remove` subcommands), kernels,
   sessions, contents, websocket v1 bridge, token auth. Validate by
   pointing VS Code at it with cdp-kernel. Then serve JupyterLab's static
   bundle as a second validation.
5. Minimal TS frontend: cells, CodeMirror, output renderers for text,
   HTML, images, markdown, errors, streams.
6. CRDT document model with `yrs` and collaborative sessions.
7. Comms, then an anywidget-only widget manager.
8. Debugger (DAP client in the UI), subshells, terminals, LSP, as desired.

Steps 1 to 4 are well-specified Rust work, roughly 15k to 25k lines for
a solid implementation. Step 4 alone is a usable product with VS Code or
JupyterLab as the frontend. Step 5 onward is where effort becomes
open-ended.

## 6. Use case: ts-on-ipados

The [ts-on-ipados](../ts-on-ipados/) design puts an iPad client in front
of kernels on the homelab over the tailnet, with a local in-process kernel
later. Its review established that remote clients speak the server's REST
API and websocket, never ZMTP, and that iCloud Drive, not SMB, is the file
sync story. Read against that plan, this server is the remote tier, and
the plan adds the following requirements.

Tier 0, remote validation. The plan runs a Jupyter server plus cdp-kernel
on the Mac mini with Juno Connect or JupyterLab in Safari as the client.
This server replaces the Python one there, which means:

- REST API fidelity. Juno Connect and JupyterLab are third-party clients
  of the API; anything nonstandard in `/api/contents`, `/api/sessions`, or
  the websocket subprotocols breaks them. The API is a compatibility
  target, not a design space, until a bespoke client exists.
- Token auth as the baseline. The review names a tokenless,
  allow-all-origins setup as the thing not to copy. An alternative worth
  having: when served through `tailscale serve`, trust the identity
  headers it injects and skip tokens. Both are small.
- Kernels survive client disconnects. The iPad suspends the app; the
  websocket drops; the kernel must keep running and the client must
  reattach by kernel id with iopub resumed. Idle culling with a
  configurable timeout is the complement, so abandoned kernels do not
  pile up.
- Files are the source of truth, not server memory. With the notebook
  root in iCloud Drive, the iPad writes files underneath the server. The
  contents API needs external-change detection (mtime check on PUT, as
  `jupyter_server` does), atomic writes (temp file plus rename, so iCloud
  never syncs a half-written notebook), and a checkpoint directory
  configurable to a path outside the synced folder.
- Runs as a daemon: launchd on the Mac mini, one config file, logs to a
  file.

Tier 1, bespoke client. The client is HTTP plus websocket in Swift. That
makes the websocket protocol and the channel-message shape a public
contract of this server. Document it as such.

Tier 2, local kernel. Two implications, both about packaging rather than
features:

- The review's point that the local kernel should speak the same
  channel-JSON shape the websocket carries, with no ZMTP, means the
  interesting boundary in this codebase is "a stream of channel messages
  to and from a kernel", with ZMQ as one transport behind it. Put that
  boundary in a library crate with no tokio-net or ZMQ dependency of its
  own, so the iPad app can compile the protocol, nbformat, and document
  crates for iOS (via uniffi or swift-bridge) and drive its in-process
  kernel through the same types. Workspace shape: `protocol`, `nbformat`,
  `document`, `kernel-host` (transport trait plus the ZMQ and in-process
  implementations), `server` (axum binary). The server is one consumer of
  the libraries; the app is another.
- The plan's development loop drives the device kernel from JupyterLab on
  the Mac through a ZMTP listener on the iPad. That needs an "attach to
  an existing kernel" provisioner: given a connection file or ip and
  ports, connect without launching. `jupyter console --existing` is the
  precedent. Cheap, and it is also how a kernel inside a container or on
  another host gets used.

Capability manifests. cdp-kernel already writes `metadata` into
`kernel.json` and JupyterLab reads `metadata.debugger` from it. The plan
adds `metadata.capabilities` and the same object in `kernel_info_reply`.
The server must pass kernelspec metadata and `kernel_info_reply` through
verbatim, with no schema that strips unknown fields. Worth a test.

A tension to decide early. Section 2.5 recommends a `yrs` CRDT document.
With iCloud as the sync story, there are two writers of the same file:
the server and the Files app. A server-side CRDT is authoritative only
for clients connected to it; the file on disk must still be re-read when
it changes underneath. Either treat the file as truth and keep the CRDT
as a live session cache that reloads on external change, or drop the
CRDT for v1 and rely on last-writer-wins with the mtime check. For this
use case, the second is enough; multi-client live collaboration is not
on its roadmap.

Out of scope for the server: the automation surface, the on-device
transform pipeline, and `tsc --noEmit` as a kernel op, which is kernel
work.

## 7. Prior art to read first

- `jupyter_client` messaging spec (v5.5) and JEP 91 (subshells), JEP 92
  (optional features / `supported_features`).
- `jupyter_server` websocket protocol doc and
  `services/kernels/connection/base.py` for the v1 binary layout.
- runtimed workspace: `jupyter-protocol`, `runtimelib`, `nbformat`,
  `jupyter-websocket-client`. Zed's `repl` crate is a consumer.
- Deno's built-in kernel (`deno jupyter`): a kernel in Rust on the
  `zeromq` crate, which is the other side of the same wire.
- `evcxr_jupyter`: Rust kernel, useful as a test target.
- `nbclient` for execution semantics, `jupyter_ydoc` for the CRDT shape.
- xeus (C++) for a non-Python view of the kernel protocol.
- cdp-kernel docs: architecture, runtimes, known gaps, and the widgets
  and comms guide. The generated `kernel.json` is the first thing to read.
- Jupyverse (David Brochart): a from-scratch `jupyter_server` replacement
  in Python. Useful for the exact endpoint list JupyterLab's frontend
  needs, including `/lab/api/*`.
- VS Code Jupyter extension: the remote-server client, for what a
  non-JupyterLab frontend actually calls.
- anywidget: the widget protocol subset to support instead of ipywidgets.
- Datalayer's `jupyter-react`: a React frontend that reuses
  `@jupyterlab/rendermime` and `@jupyterlab/outputarea` without the rest
  of JupyterLab. Relevant to UI option 1.
