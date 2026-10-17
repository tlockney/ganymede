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
