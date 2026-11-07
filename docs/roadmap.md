# Roadmap

What is done, and what comes next, in order. Each step lands as its own
pull request with a GitHub issue, and records what it learned in the
[development log](notes.md).

## Done

- **Core crates.** Protocol, kernel host, nbformat, executor, contents,
  API, and server, with the `ganymede` CLI and the embeddable
  `ganymede-engine`.
- **Parity with Jupyter.** An end-to-end suite runs cdp-kernel and
  ipykernel through Ganymede and compares the results with
  `jupyter execute` and `jupyter server`.
- **Existing frontends.** The VS Code Jupyter extension's remote-server
  flow passes. JupyterLab's own bundle is served at `/lab` with its
  `/lab/api/*` endpoints and runs notebooks in a browser, with cookie
  login as `jupyter_server` does it.
- **Portability.** The engine builds for iOS, Android, and wasm, and runs
  a notebook inside wasm under WASI.
- **Kernel conformance.** cdp-kernel (Node 24) and ipykernel run the same
  scenarios in CI: launch, execute, interrupt, restart, comms with binary
  buffers, stdin, aborted queues, history, display updates, the runtime
  exiting mid-cell, parallel starts, the debugger over control, and a
  malformed stdin frame, with kernelspec metadata and `kernel_info_reply`
  extras passed through untouched. Through `serve`, parallel starts, a crash,
  and a restart that fails are run on real kernels, and a weekly job runs the
  scenarios against cdp-kernel's `main`.
- **Kernel lifetime.** Kernels survive websocket drops, and a client that
  reconnects with the same `session_id` gets the output it missed, as
  `jupyter_server` buffers it. Idle culling is configurable. A kernel that
  crashes is restarted under the same id, as `jupyter_server`'s restarter
  does, and reported `dead` and removed after five crashes in a row; an
  attached one is watched by heartbeat. A start or restart whose kernel dies or doesn't
  answer is relaunched on fresh ports, up to three tries. Shutdown sends
  `shutdown_request`, then SIGTERM, then SIGKILL, and kernels die with the
  host.
- **JupyterLab without Python.** `ganymede lab install` fetches
  JupyterLab's prebuilt bundle from PyPI, `ganymede lab extension install`
  the prebuilt extensions (the widget manager and [anywidget](https://github.com/manzt/anywidget) by default),
  and `ganymede lab` serves and opens it.
- **Releases and install.** A `vX.Y.Z` tag builds and publishes binaries
  for Linux, macOS, and Windows (the Windows build is not tested beyond
  compiling). `install.sh` installs a release and cdp-kernel. CI's
  `workstation` job runs the documented install on a machine with no
  Python and drives JupyterLab with Playwright.
- **Notebook trust.** Notebooks are signed on save and marked `trusted`
  on read as `jupyter_server` does, with Jupyter's key and signature
  database, and `ganymede trust` signs one by hand.
- **A UI of our own.** [ganymede-ui](https://github.com/tlockney/ganymede-ui)
  is a notebook app built on JupyterLab's packages, outside the core. It
  runs in a browser against Ganymede or with kernels in the page, and in
  a Mac and iPad app that runs the core in-process through
  `ganymede-ffi`.
- **Remote kernels.** `--gateway-url` runs kernels on another Jupyter or
  Ganymede server while files stay local, which is how the iPad app gets
  kernels it can't launch.
- **nbformat 3.** Older notebooks are upgraded to 4.5 on read.
- **Terminals and language servers.** `/api/terminals` over a
  pseudo-terminal, and `jupyter-lsp`'s relay to language servers from the
  config file, both with no Python
  ([JupyterLab and VS Code](frontends.md#terminals)).
- **Document rooms.** The file is the source of truth: a room holds the
  live notebook as a `yrs` document, reloads it when the file changes on
  disk, merges outside changes cell by cell, and runs cells into it
  through the execute endpoint
  ([Document model and collaboration](protocol.md#document-model-and-collaboration)).
- **Agents.** `ganymede mcp` gives coding agents notebook tools, and the
  ganymede-journal plugin for Claude Code keeps an analysis as a notebook
  while the agent works ([Agents over MCP](mcp.md)).
- **Observability.** A kernel's log (`/api/kernels/{id}/log`),
  `jupyter-resource-usage`'s memory and CPU metrics, and opt-in crash
  reports with no notebook content ([Privacy](privacy.md)).
- **Signed apps.** The Mac app is signed and notarized in each ganymede-ui
  release, and the iPad and iPhone app is on TestFlight for internal
  testers.

## Next

1. **ganymede-ui on document rooms.** The app opens notebooks through
   rooms, so edits from an agent, another window or another device arrive
   live, and shows the agent's journal as it is written
   ([ganymede-ui#183](https://github.com/tlockney/ganymede-ui/issues/183),
   [#184](https://github.com/tlockney/ganymede-ui/issues/184)).
2. **A Python kernel in the apps.** Pyodide, run in the page like the
   in-app TypeScript kernel, so the iPad and iPhone app runs Python
   notebooks with no server
   ([ganymede-ui#258](https://github.com/tlockney/ganymede-ui/issues/258),
   [#375](https://github.com/tlockney/ganymede/issues/375)). Python stays
   out of the product: it is a kernel the page loads.

## Open

- A native on-device kernel for the iPad app: cdp-kernel in the app's
  process instead of the web view, behind the `Provisioner` and
  `TransportFactory` seams that already accept a kernel with no ZeroMQ.
- CurveZMQ for the ZeroMQ transport. ipykernel 7 already advertises it.
- Trusting `tailscale serve`'s identity headers, so a client on the
  tailnet needs no token.
- A Linux build for musl (Alpine).
- Distributing the iPad and iPhone app beyond internal TestFlight
  testers, which needs App Review (rule 2.5.2, apps that run code). The
  Mac app is released, signed and notarized.
- A runtime seam for the browser (timers and `spawn` on
  `wasm-bindgen-futures`) and a websocket or `postMessage` transport, so
  the engine runs in a page and not only under WASI.
- Windows interrupts, which ipykernel does through a named event rather
  than a signal.

## Not planned

JupyterLab server extensions and `jupyter_server` extensions (prebuilt
JupyterLab frontend extensions do work), nbconvert's template exports
(HTML, PDF, LaTeX, slides), and the
JupyterHub single-user contract. See [Replacing Python](python-free.md)
for why.
