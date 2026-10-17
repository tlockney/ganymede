# Prior art

What to read before working on a part of Ganymede.

## Reading list

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
  of JupyterLab. Relevant to a TypeScript web UI.
- JupyterLite: JupyterLab in the browser with a TypeScript reimplementation
  of the REST API in a service worker and in-browser kernels. The proof
  that the frontend can be pointed at a non-HTTP backend through
  `ServerConnection`, and the closest precedent for the iPad profile.
- `jupyter_server`'s gateway client and Jupyter Kernel Gateway: the
  remote provisioner pattern.

## The reference stack by size

Jupyter's components, with rough Python line counts, and the Rust
equivalents that existed when Ganymede started:

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
