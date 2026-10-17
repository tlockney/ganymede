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
