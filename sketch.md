# Sketch: a Rust reimplementation of the Jupyter core

Scope as first stated: a kernel host, a presentation layer (web UI), and the
essential protocol. This note breaks that into components, estimates each, lists
the parts that are usually missed, and proposes an order of work.

Clarified scope (2026-10-17): Rust is for the core server only: protocol,
kernel host, nbformat, contents, and the API. The UI is not part of the core
and is not Rust. It is a pluggable client of the core's public API, chosen per
platform: an existing Jupyter frontend, a TypeScript web UI, or a native shell
such as a Swift app on iPadOS that hosts a web view for rendering.

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

The UI is outside the Rust core. It is a client of the core's API (REST
plus websocket, or the same JSON shapes over an in-app bridge), and more
than one UI can exist. This section lists what any notebook UI needs,
whatever it is written in.

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

UI implementations, each a separate client of the same API:

1. Existing Jupyter frontends: VS Code, JupyterLab's static bundle, Juno
   Connect. No UI work; the API is the contract they test.
2. A TypeScript web UI. Reuse CodeMirror, KaTeX, and optionally
   `@jupyterlab/rendermime` and `@jupyterlab/outputarea` as libraries
   without the rest of JupyterLab. Runs in a browser, or inside a native
   shell's web view.
3. A native shell with a web view for rendering, such as a Swift app on
   iPadOS with `WKWebView`. The shell owns navigation, files, keyboard, and
   app lifecycle; the web view runs the renderers from option 2, since
   `text/html` outputs, math, Vega, Plotly, and widgets are JavaScript.
   The shell talks to the core over HTTP, or in-process through a bridge.
4. Fully native rendering (SwiftUI, or GPUI as Zed does). Loses
   `text/html` output, which breaks pandas, plotly, bokeh, altair, and
   widgets. Fine for a REPL, not for a general notebook.

What the core owes every UI: a documented, stable API, and nothing that
assumes which UI is on the other end. Rust UI frameworks (Leptos, Dioxus,
Yew, egui) are out of scope.

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
5. A first UI of our own, outside the core: TS renderers (text, HTML,
   images, markdown, errors, streams) and CodeMirror, usable in a browser
   or inside a native shell's web view.
6. CRDT document model with `yrs` and collaborative sessions.
7. Comms, then an anywidget-only widget manager.
8. Debugger (DAP client in the UI), subshells, terminals, LSP, as desired.

Steps 1 to 4 are well-specified Rust work, roughly 15k to 25k lines for
a solid implementation. Step 4 alone is a usable product with VS Code or
JupyterLab as the frontend. Step 5 onward is where effort becomes
open-ended.

## 6. Use case: ts-on-ipados

The [ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados) design puts an iPad client in front
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

## 7. Modular architecture and deployment profiles

Goal, restated: one set of modules that compose into a Jupyter-like
environment on an iPad, a desktop, or a remote server. TypeScript kernels
are the focus. The host must fit the Jupyter ecosystem well enough that
on a desktop or server it can also run a Python kernel, without Python
ever being an assumption.

### 7.1 The one rule

The host knows no language. Every language-specific fact comes from three
places: the kernelspec (`language`, `metadata`), `kernel_info_reply`
(`language_info`, `supported_features`, extra fields), and MIME types on
outputs. A grep for `python` in the host crates, outside tests and the
conformance fixtures, should find nothing. The test suite runs the same
scenarios against cdp-kernel and ipykernel, and both must pass.

### 7.2 Seams

Each seam is a trait in Rust or an interface in TypeScript. The right
column lists the implementations needed across the three profiles.

| Seam | What it abstracts | Implementations |
|---|---|---|
| `KernelTransport` | A stream of channel messages to and from one kernel, with channel name, header, content, buffers | ZMQ (desktop, server); in-process channel (iPad local kernel); websocket client to another host (iPad to server, or any host proxying a remote host) |
| `Provisioner` | How a kernel comes to exist | Local subprocess from a kernelspec; attach to an existing connection file or address; in-process (the app owns the kernel); remote, which proxies to another host's REST API as `jupyter_server`'s gateway client does |
| `KernelspecSource` | Where the list of launchable kernels comes from | Jupyter data dirs on the filesystem; extra roots from config, which is how conda and venv kernels get found without the host knowing what a venv is; an environment scanner as an optional module, as VS Code has; a static embedded list for the iPad (local kernel plus configured remotes) |
| `Contents` | Files, directories, checkpoints | Local filesystem with atomic writes and mtime checks; Files app or iCloud container on iPad; passthrough to a remote host |
| `Auth` | Who may call the API | None (in-app bridge); token; `tailscale serve` identity headers; later, an OIDC or Hub contract |
| `Frontend host` | How a UI reaches the core | HTTP plus websocket (axum) on desktop and server; an in-app bridge on iPad that carries the same JSON shapes with no HTTP |
| UI (any language) | The client the user sees | Existing Jupyter frontends; a TS web UI; a Swift shell with a web view on iPad. Not a Rust module; the core only owes it the API |
| `Document` | The live notebook model | File-as-truth with reload on external change; `yrs` CRDT with y-websocket for live multi-client sessions |
| `Renderer` (TS) | One MIME type to DOM | Text, ANSI streams, HTML (sanitized or iframed), images, SVG, Markdown, LaTeX, JSON, Vega, Plotly, and a widget view |
| `WidgetManager` (TS) | The `jupyter.widget` comm target | anywidget-only, small, enough for cdp-kernel; full `@jupyter-widgets/base` plus `controls`, needed when a Python kernel with ipywidgets runs. Choose per profile, not globally |
| `Editor` (TS) | Code editing | CodeMirror 6, mode from `language_info.codemirror_mode` |

Two of these seams already exist in the ecosystem and should be matched
rather than invented. On the server side, `jupyter_server`'s gateway
client is the remote provisioner. On the frontend side,
`@jupyterlab/services` takes a `ServerConnection` with overridable `fetch`
and `WebSocket`, which is exactly how JupyterLite points JupyterLab at an
in-browser backend with no HTTP server. The iPad profile can use the same
trick: the TS frontend in a web view, the core in Rust behind a message
bridge that implements `fetch` and `WebSocket`. The shell around that web
view is a Swift app; it can also call the core directly through the `ffi`
crate for things the web view does not need to see.

### 7.3 Profiles

| Module | Server (homelab) | Desktop | iPad |
|---|---|---|---|
| Transport | ZMQ | ZMQ | In-process; websocket client to server |
| Provisioner | Subprocess, attach, remote | Subprocess, attach, remote | In-process, remote |
| Kernelspecs | Filesystem plus config roots | Filesystem plus config roots | Embedded list |
| Contents | Filesystem | Filesystem | Files app or iCloud container |
| Auth | Token or tailscale headers | Loopback token | None, in-app |
| Frontend host | HTTP | HTTP, or the core in-process in a native app | In-process bridge |
| UI | Any Jupyter client; optionally serves a TS bundle | Same, or a native shell with a web view | Swift shell with `WKWebView` for rendering |
| Document | File-as-truth; CRDT optional | File-as-truth | File-as-truth |
| Widget manager | Full, if Python kernels are expected | Full | anywidget-only |
| Kernels seen | cdp-kernel, python3, anything installed | Same | Local TS kernel, plus whatever the remote host lists |

The iPad profile has no ZMQ, no subprocesses, no HTTP server, and no
Python. The server profile has all of them except Python, which arrives
only as a kernelspec someone installed. The same crates, different
feature flags and a different binary or app shell.

Crate layout that makes this true:

- `protocol`: message types, serde, HMAC. No I/O.
- `nbformat`: file format, validation, round-trip writer. No I/O.
- `kernel-host`: `KernelTransport`, `Provisioner`, `KernelspecSource`
  traits; the kernel session state machine (nudge, busy/idle tracking,
  reply routing, multi-client fan-out, restart policy). Depends on
  `protocol` only. Implementations behind feature flags: `zmq`,
  `subprocess`, `in-process`, `remote`.
- `contents`: trait plus filesystem implementation.
- `document`: file-as-truth model; `crdt` feature adds `yrs`.
- `api`: the REST and websocket handlers as functions over the traits,
  independent of axum, so the iPad bridge and the HTTP server call the
  same code.
- `server`: axum binary, config, auth, static assets.
- `ffi`: uniffi or swift-bridge surface for the app.

UIs live outside this crate list. The TS packages are shared by any UI
that renders in a web view: `connection` (the `ServerConnection` shim),
`renderers`, `widgets-anywidget`, `widgets-full`, `editor`. Each UI adds
its own shell on top: a browser `app`, or a Swift app on iPad.

### 7.4 What "fits the Jupyter ecosystem" means, ranked

In order of value per unit of work:

1. **Kernel compatibility.** Protocol 5.x over ZMQ plus the kernelspec
   convention. Every existing kernel works: cdp-kernel, ipykernel, IRkernel,
   IJulia, evcxr, Deno, xeus. This is the core and is non-negotiable.
2. **File compatibility.** nbformat 4.5 with faithful round-trip. Notebooks
   interchange with git, GitHub rendering, nbviewer, nbconvert run elsewhere,
   and every other client. Core.
3. **Tooling conventions.** Jupyter data dirs, runtime dir and connection
   file naming, `JUPYTER_*` environment variables. Cheap, and it means
   `jupyter console --existing`, kernel installers, and environment tools
   all keep working around the host.
4. **Client compatibility.** The `jupyter_server` REST API and websocket
   subprotocols. JupyterLab, Notebook 7, VS Code, Juno Connect, and
   papermill-style tools connect. Medium cost; server and desktop profiles
   only.
5. **Output and widget compatibility.** The standard MIME types, comms,
   the `jupyter.widget` protocol. anywidget-only is small; full ipywidgets
   is a dependency on the standard JS manager, taken only in profiles
   that expect Python kernels.
6. **Debugger.** DAP over the control channel, a DAP client in the UI.
   cdp-kernel and ipykernel both support it.
7. **Extensions.** JupyterLab frontend extensions and `jupyter_server`
   extensions. Not compatible, by design. State this in the README so
   nobody expects it.

Items 1 to 3 are what make "might also run Python" free: a Python kernel
is just another kernelspec, and ipywidgets is just another widget manager
module.

## 8. Prior art to read first

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
- JupyterLite: JupyterLab in the browser with a TypeScript reimplementation
  of the REST API in a service worker and in-browser kernels. The proof
  that the frontend can be pointed at a non-HTTP backend through
  `ServerConnection`, and the closest precedent for the iPad profile.
- `jupyter_server`'s gateway client and Jupyter Kernel Gateway: the
  remote provisioner pattern.
