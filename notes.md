# Notes

## 2026-10-17

Starting point: question of what a Rust reimplementation of the Jupyter
core (kernel host, web UI, protocol) would take and what is easy to miss.

Wrote [`sketch.md`](sketch.md): components, what already exists in Rust,
commonly missed pieces, and an order of work.

Findings while writing it:

- Messaging spec is now v5.5: `subshell_id` in headers, iopub moved to
  XPUB with an `iopub_welcome` message, `supported_features` in
  `kernel_info_reply` (JEP 92).
- The runtimed crates (`jupyter-protocol`, `runtimelib`, `nbformat`) are
  maintained and used by Zed's REPL. They cover protocol, ZMQ kernel
  client, and the file format. A new project should either build on them
  or use them as a conformance oracle.
- No Rust equivalent of `jupyter_server` exists. That plus the frontend
  is the actual work.
- The frontend cannot usefully be Rust-native if `text/html` outputs and
  widgets matter. TypeScript UI over a Rust server is the pragmatic shape.
  (Superseded by the scope clarification at the end of these notes: the
  UI was never meant to be Rust, and is pluggable.)

Open questions:

- Is the goal a product, or understanding the system? Changes whether to
  reuse the runtimed crates.
- Are widgets in scope? Largest single cost after the UI itself.
- Windows support?
- Must existing clients (JupyterLab, VS Code, papermill) connect to it?
  If yes, the REST and websocket surface is fixed by `jupyter_server`.

Nothing built.

## 2026-10-17, later

Goal clarified: a Jupyter server with no Python anywhere in the product,
kernel-agnostic, with [cdp-kernel](https://cdp-kernel.org) (TypeScript on
Node or Deno) as the first target. May become a project of its own.

Read cdp-kernel's install, architecture, and known-gaps pages. Relevant
facts:

- Speaks ZMTP over TCP via `node:net`; no `ipc` transport.
- Installs kernelspecs to the standard Jupyter data dirs
  (`$JUPYTER_DATA_DIR`, `~/Library/Jupyter`, XDG). Uninstall docs still
  point at the Python `jupyter kernelspec` CLI, so the server should ship
  `kernelspec list/install/remove`.
- Interrupts over CDP internally; `kernel.json` contents (including
  `interrupt_mode`) are not documented. Read the generated file.
- anywidget-style widgets and comms only; no ipywidgets controls.
- Debugger on Node and Deno targets; needs a DAP client in the UI.
- `prompt()` and `confirm()` over stdin.
- Kernel inherits the server's environment. Windows unsupported.

Changes to `sketch.md`: revised goal paragraph, new section 3 (table of
Python assumptions in the reference stack and their replacements, what
cdp-kernel needs from a server, frontends that work today), order of work
now validates the server with VS Code plus cdp-kernel before any UI, and
more prior art (Jupyverse, VS Code Jupyter extension, anywidget,
Datalayer jupyter-react).

Main conclusion: step 4 (the server) is a usable product on its own.
VS Code's Jupyter extension against a Rust server running cdp-kernel is
a fully Python-free TypeScript notebook environment with no UI work.

Open questions, updated:

- Build on the runtimed crates or reimplement? Still open.
- Which `interrupt_mode` does cdp-kernel declare?
- Serve JupyterLab's static bundle (needs `/lab/api/*`) or target VS Code
  only for v1?
- Widgets: anywidget-only manager in a custom UI, or rely on JupyterLab
  and VS Code's managers until a UI exists?

## 2026-10-17, ts-on-ipados as a use case

The [ts-on-ipados](../ts-on-ipados/) plan is one intended use: this
server is its remote tier on the Mac mini over the tailnet. Read the
design note sections 04, 05, 09 and the review sections 3, 4, 6, 7.
Added section 6 to `sketch.md`.

What the plan adds to the server's requirements:

- REST API fidelity, because Juno Connect and JupyterLab in Safari are
  the Tier 0 clients.
- Token auth baseline; `tailscale serve` identity headers as an option.
- Kernels survive websocket drops (iPad suspension) and clients reattach
  by id; idle culling as the complement.
- Files are truth: iCloud Drive writes under the server. Needs mtime
  conflict check on PUT, atomic writes, checkpoint dir outside the synced
  folder.
- The websocket and channel-message shape become a public contract once
  the Swift client exists.
- Library-first workspace so the iPad app can compile the protocol,
  nbformat, and document crates for iOS and drive its in-process kernel
  through the same types. ZMQ is one transport behind a trait.
- An attach-to-existing-kernel provisioner, for driving the device kernel
  from a desktop JupyterLab during development.
- Pass kernelspec `metadata` and `kernel_info_reply` through verbatim so
  capability manifests reach the client.

Tension noted: a server-side CRDT and iCloud file sync are two writers
of one file. For this use case, file-as-truth with the mtime check is
enough for v1.

## 2026-10-17, modules and profiles

Goal restated: pluggable modules composing a Jupyter-like environment on
iPad, desktop, or server; TypeScript kernels as the focus; Python kernels
possible on desktop and server but never assumed. Added section 7 to
`sketch.md`: the one rule (the host knows no language), the seams with
their implementations, the three profiles, a crate layout, and a ranked
definition of ecosystem compatibility.

Decisions implied:

- Kernel host boundary is "a stream of channel messages", with ZMQ,
  in-process, and websocket-client transports behind one trait.
- The REST and websocket handlers are functions over traits, not axum
  handlers, so the iPad bridge and the HTTP server share them.
- The widget manager is per profile: anywidget-only on iPad, the full
  ipywidgets manager where Python kernels are expected.
- Extension compatibility (JupyterLab, jupyter_server) is out, by design.

Found two existing seams to match: `jupyter_server`'s gateway client
(remote provisioner) and `@jupyterlab/services` `ServerConnection` with
custom `fetch` and `WebSocket` (how JupyterLite runs without a server).
JupyterLite is the closest precedent for the iPad profile.

Test rule to adopt: the same scenarios run against cdp-kernel and
ipykernel; no `python` string in host crates outside tests.

## 2026-10-17, code sketch

Started the code. Cargo workspace at the project root, `crates/*`, and a
TypeScript package under `frontend/`. Everything builds, tests pass,
clippy is clean. Test oracle: a scratch venv with ipykernel 7 and
nbformat 5.11 (cdp-kernel needs Node 24; this container has 22).

Crates:

- `jcore-protocol`: envelope, typed content with `extra` passthrough,
  ZMQ wire framing with HMAC-SHA256, connection file, websocket v1 and
  legacy codecs. No I/O. 12 tests, including an HMAC vector checked
  against Python and the v1 offset layout from `jupyter_server`.
- `jcore-nbformat` (written by a subagent): nbformat 4.5 types, lenient
  reader, byte-exact writer matching `nbformat.write`. 24 tests against
  fixtures generated by Python nbformat.
- `jcore-kernel-host`: kernelspec discovery over the Jupyter data dirs,
  the transport seam with an in-process pair, the ZMQ transport on the
  pure-Rust `zeromq` crate, subprocess and attach provisioners, the
  session state machine, and a fake kernel. 9 tests.
- `jcore-exec`: headless executor over the two above. 2 tests.
- `jcore-cli`: `jcore kernelspec list|paths`, `jcore run`, `jcore attach`,
  `jcore exec`.

Verified against ipykernel over real ZMQ:

- `jcore run --kernel python3 ...`: launch, nudge, five cells, shutdown,
  about 1.6 s total.
- `--interrupt-after-ms`: SIGINT to the process group interrupts a
  sleeping cell; `interrupt_request` on control does the same on the fake.
- `jcore attach <connection file>` against a kernel started by
  `jupyter_client`.
- `jcore exec`: a notebook generated by nbformat with streams, a
  `display_id` update, `clear_output(wait=True)`, and an empty cell. The
  output validates with nbformat and is byte-identical after an nbformat
  rewrite.

Things learned:

- `tokio::sync::watch::Sender::send` fails when no receiver exists; use
  `send_replace` for state that may have no watchers yet.
- ipykernel 7 reports protocol 5.3 in `kernel_info_reply` while its
  kernelspec says 5.5. Do not gate on the version string.
- ipykernel warns about unencrypted TCP and advertises
  `supported_encryption: ["curve"]` in kernelspec metadata. CurveZMQ is a
  future item for the ZMQ transport.
- Python `json.dumps` float layout (`1e-05`, `2.5e-07`) differs from
  serde_json; the nbformat writer has its own float formatter.
- `nbformat.write` assigns ids to cells that lack them, so byte fidelity
  is only defined for notebooks with ids.

Frontend (written by a subagent): protocol types, the websocket v1 and
legacy codecs, a `ServerConnection` with injectable `fetch` and
`WebSocket`, a REST client, a `KernelConnection` with the two-part
completion rule, a renderer registry, ANSI and stream handling, and
widget manager interfaces. 45 tests on Node's test runner, `typescript`
as the only dependency. No editor, no DOM tests, no app shell yet.

Not done, in the order the sketch suggests: the server crate (REST and
websocket bridge over the session), the contents crate, the document
model, an app shell for the frontend, cdp-kernel verification (needs
Node 24), CurveZMQ, Windows.

## 2026-10-17, server crate

Added three crates. `jcore-contents` (subagent): the contents seam and a
filesystem implementation with atomic writes, the mtime conflict check,
checkpoints with a configurable directory outside the root, and path
safety including symlink escapes; 30 tests. `jcore-api`: kernel and
session managers, models, and the per-client bridge, with no HTTP; 6
tests. `jcore-server`: axum routes, token auth, websocket channels, TOML
config, static assets; 5 integration tests including a real websocket
round trip against the fake kernel.

Verified against ipykernel over ZMQ, through HTTP, from Node:

- Our `frontend/` client: kernelspecs, start, kernel_info, execute with
  stdout and stderr, result, error reply, shutdown.
- `@jupyterlab/services` 7.5, the real JupyterLab client library: specs,
  start, info, execute, complete, signal interrupt of a sleeping cell,
  restart (it reconnects its websocket on its own), contents newUntitled,
  save, get, list, sessions bound to the kernel, shutdown, and the server's
  close frame when the kernel is gone.

Things learned:

- JupyterLab's client POSTs JSON without `Content-Type`; axum's `Json`
  extractor answers 415. `jupyter_server` parses the body regardless. The
  server now has a lenient extractor for every body route.
- A client holding an `Arc` to the running kernel keeps the broadcast hub
  alive, so "hub closed" can never signal shutdown; an explicit alive flag
  the client selects on does.
- `KernelSession::shutdown` must not wait on or kill a process it does
  not own; attached kernels have no pid.
- A kernel started with no path must run in the contents root, not the
  server's cwd.
- `pkill -f <pattern>` matches the shell running it. Use process names.
- The contents crate's checkpoint layout with an external directory
  mirrors the root's tree instead of jupyter_server's flat folder, so
  same-named files in different directories do not collide.

Not done: cookie login for a browser frontend, serving JupyterLab's static
bundle (needs `/lab/api/*`), CurveZMQ, kernel autorestart on crash,
cdp-kernel verification.

## Scope clarification: the UI is not Rust (2026-10-17)

Only the core server is Rust. The UI was never meant to be, and should
itself be pluggable: any Jupyter frontend, a TypeScript web UI, or a
native shell, for example a Swift app on iPadOS with a web view doing the
rendering. Earlier drafts read as if the presentation layer was part of
the Rust project (scope line, section 2.6 weighing Rust UI frameworks,
`frontend/` described as "the" frontend). The sketch, README, and section
7 now treat the UI as a client of the core's API, with the TS package as
shared rendering code for any web-view UI. No code changes: the Rust
crates never depended on a UI.

