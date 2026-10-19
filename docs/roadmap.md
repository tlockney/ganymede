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
  exiting mid-cell, and parallel starts, with kernelspec metadata and
  `kernel_info_reply` extras passed through untouched.
- **Kernel lifetime.** Kernels survive websocket drops, and a client that
  reconnects with the same `session_id` gets the output it missed, as
  `jupyter_server` buffers it. Idle culling is configurable. A kernel that
  dies is reported `dead` to its clients and removed; an attached one is
  watched by heartbeat. A start or restart whose kernel dies or doesn't
  answer is relaunched on fresh ports, up to three tries. Shutdown sends
  `shutdown_request`, then SIGTERM, then SIGKILL, and kernels die with the
  host.
- **JupyterLab without Python.** `ganymede lab install` fetches
  JupyterLab's prebuilt bundle from PyPI, `ganymede lab extension install`
  the prebuilt extensions (the widget manager and anywidget by default),
  and `ganymede lab` serves and opens it.
- **Releases and install.** A `vX.Y.Z` tag builds and publishes binaries
  for Linux, macOS, and Windows (the Windows build is not tested beyond
  compiling). `install.sh` installs a release and cdp-kernel. CI's
  `workstation` job runs the documented install on a machine with no
  Python and drives JupyterLab with Playwright.
- **Notebook trust.** Notebooks are signed on save and marked `trusted`
  on read as `jupyter_server` does, with Jupyter's key and signature
  database, and `ganymede trust` signs one by hand.

## Next

1. **Document model.** The file is the source of truth and is reloaded
   when it changes on disk (iCloud). A `yrs` CRDT layer is optional and
   later.
2. **A first UI of our own**, outside the core: a Swift shell with a web
   view for iPadOS and macOS, running a TypeScript notebook app, with the
   Rust core embedded in the same process. The TypeScript app is
   [ganymede-ui](https://github.com/tlockney/ganymede-ui), a prototype that
   runs in a browser and installs as a PWA.
3. **CurveZMQ** for the ZeroMQ transport. ipykernel 7 already advertises
   it.

## Open

- Restarting a kernel automatically when it crashes, as
  `jupyter_server`'s restarter does. Today a dead kernel is reported and
  removed.
- Conformance gaps: debugger (`debug_request`) messages, malformed stdin
  frames, and kernel death and parallel starts driven through `serve`.
- A Linux build for musl (Alpine).
- A runtime seam for the browser (timers and `spawn` on
  `wasm-bindgen-futures`) and a websocket or `postMessage` transport, so
  the engine runs in a page and not only under WASI.
- Windows interrupts, which ipykernel does through a named event rather
  than a signal.

## Not planned

JupyterLab server extensions and `jupyter_server` extensions (prebuilt
JupyterLab frontend extensions do work), nbconvert export, and the
JupyterHub single-user contract. See [Replacing Python](python-free.md)
for why.
