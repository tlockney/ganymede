# User interfaces

The UI is outside the Rust core. It is a client of the core's API (REST
plus websocket, or the same JSON shapes over an in-app bridge), and more
than one UI can exist. This page lists what any notebook UI needs,
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
  untrusted and must be sanitized until re-executed. The server keeps the
  signatures (Jupyter's `nbsignatures.db`) and marks code cells
  `metadata.trusted` when it serves a notebook; a UI sends `trusted: true`
  on cells whose output it produced, and `POST /api/contents/{path}/trust`
  trusts a notebook on request.

UI implementations, each a separate client of the same API:

1. Existing Jupyter frontends: VS Code, JupyterLab's static bundle, Juno
   Connect. No UI work; the API is the contract they test.
2. A TypeScript web UI. Reuse CodeMirror, KaTeX, and optionally
   `@jupyterlab/rendermime` and `@jupyterlab/outputarea` as libraries
   without the rest of JupyterLab. Runs in a browser, or inside a native
   shell's web view. [ganymede-ui](https://github.com/tlockney/ganymede-ui)
   is a prototype of this one, built on JupyterLab's notebook through
   datalayer/jupyter-ui.
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
