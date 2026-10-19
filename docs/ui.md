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

The UIs, each a separate client of the same API:

1. **Existing Jupyter frontends**: JupyterLab's static bundle, VS Code,
   and [others](clients.md). No UI work; the API is the contract they
   test, and [JupyterLab and VS Code](frontends.md) shows how to connect
   them.
2. **ganymede-ui**, a TypeScript web app built on JupyterLab's notebook
   packages used as libraries, without the rest of JupyterLab. It runs in
   a browser, and in the Mac and iPad app: a Swift shell that owns
   windows, files and the app lifecycle, runs Ganymede in-process, and
   shows ganymede-ui in a `WKWebView`. Rendering stays in the web view
   because `text/html` outputs, math, Vega, Plotly and widgets are
   JavaScript. See [Ganymede's app](app.md) for using it and its
   [architecture](ganymede-ui/architecture.md) for how it is built.
3. **Fully native rendering** (SwiftUI, or GPUI as Zed does). Not
   pursued: it loses `text/html` output, which breaks pandas, plotly,
   bokeh, altair, and widgets. Fine for a REPL, not for a general
   notebook.

What the core owes every UI: a documented, stable API, and nothing that
assumes which UI is on the other end. Rust UI frameworks (Leptos, Dioxus,
Yew, egui) are out of scope.
