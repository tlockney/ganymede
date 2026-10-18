# frontend

TypeScript side of Ganymede: the module seams and the protocol
plumbing described in [`docs/ui.md`](../docs/ui.md) and the seams in
[`docs/architecture.md`](../docs/architecture.md), for a UI of Ganymede's
own (step 2 of the [roadmap](../docs/roadmap.md)). It is not used by the
server today: `ganymede lab` serves JupyterLab, whose own widget manager
renders widgets. Here there is still no editor, no widget implementation,
and no UI framework. The layout mirrors the Rust crates.

## Run

```
cd frontend
npm install
npm test        # tsc, then node --test over dist/**/*.test.js
npm run check   # tsc --noEmit
```

Node 22 or later; CI runs it on Node 24. The only dependency is `typescript` (dev). Node's built-in test
runner is used; `src/node-shims.d.ts` declares the few `node:test` /
`node:assert` shapes the tests need so `@types/node` is not required.

## Modules

| Path | Status | What it is |
|---|---|---|
| `src/protocol/messages.ts` | sketched | Message envelope (header, parent_header, metadata, content, buffers), `Channel`, typed content for the shell/iopub/stdin/comm messages a notebook needs, `createMessage`. Every content type has an index signature so unknown fields (e.g. a capability manifest in `kernel_info_reply`) survive. |
| `src/protocol/websocket-v1.ts` | sketched, tested | Encoder/decoder for the `v1.kernel.websocket.jupyter.org` binary framing, byte-for-byte as jupyter_server's `serialize_msg_to_ws_v1`. Legacy JSON codec (`channel` key at top level; buffers unsupported, throws). |
| `src/connection/server-connection.ts` | sketched, tested | `ServerConnection` settings (`baseUrl`, `wsUrl`, `token?`, `fetch`, `WebSocket`) with `makeSettings` defaulting to globals, so the iPad bridge can inject its own `fetch` and `WebSocket`. `RestClient` over `/api/kernelspecs`, `/api/kernels`, `/api/kernels/{id}/{interrupt,restart}`, `/api/sessions`, `/api/contents/{path}` with the `Authorization: token` header. Response types are loose. |
| `src/connection/kernel-connection.ts` | sketched, tested | `KernelConnection` over `/api/kernels/{id}/channels`: `send`, `onMessage`, `requestReply` (match on parent `msg_id`), `execute` (iopub listener plus a `done` that waits for both `execute_reply` and the matching `status: idle`), `inputReply` for stdin, busy/idle tracking. Reconnect is a TODO. |
| `src/renderers/registry.ts` | sketched, tested | `MimeRenderer` and `RendererRegistry` with `preferredMimeType` by rank. Default order: widget view, text/html, image/svg+xml, image/png, image/jpeg, text/markdown, text/latex, application/json, text/plain. |
| `src/renderers/ansi.ts` | sketched, tested | Pure ANSI SGR parser to `{text, classes}` spans with `\r` overwrite and `\b` delete semantics. |
| `src/renderers/stream.ts` | sketched, tested | `StreamBuffer`: merges consecutive chunks of the same stream and applies `\r`/`\b` incrementally. Keeps the raw text too, for `parseAnsi` and for the notebook file. |
| `src/renderers/basic.ts` | stubbed, untested (DOM) | text/plain (pre with ANSI spans), image/png and image/jpeg (data URL), image/svg+xml, text/html. HTML and inline SVG only go into the document when `trusted`; otherwise HTML is shown as text and SVG goes through `<img>`. Sanitizing is a TODO (DOMPurify). |
| `src/widgets/manager.ts` | interfaces only | `WidgetManager` seam for the `jupyter.widget` comm target, with notes on the two planned implementations: anywidget-only and full ipywidgets. |
| `src/index.ts` | | Re-exports the public surface. |

Not started: `editor/` (CodeMirror 6, mode from `language_info.codemirror_mode`),
`app/` (cell list, output area, kernel picker, file browser), markdown and
LaTeX renderers, `clear_output(wait)` and `update_display_data` handling,
output truncation and iopub rate limiting, the widget managers.

## Protocol details worth knowing

- v1 frame: `offset_number` (u64 LE), then that many u64 LE offsets, then the
  channel name, then header/parent_header/metadata/content JSON, then each
  buffer. `offsets[0] = 8 * (1 + offset_number)` is where the channel starts;
  `offsets[1]` is where the first part starts; the last offset equals the
  frame length. All lengths are in UTF-8 bytes (tests cover non-ASCII).
- The token goes in the websocket URL as `?token=` because browsers cannot
  set handshake headers; `session_id` is also a query parameter, as
  `@jupyterlab/services` does it.
- If the server answers with text frames (no subprotocol negotiated) the
  connection switches to the legacy JSON codec for sending as well.
- `\b` deletes the previous character rather than just moving the cursor,
  following JupyterLab's `fixBackspace`, since that is what notebook users
  expect from tqdm-style output.
- Untrusted `text/html` is displayed as text, not sanitized. That is the
  conservative placeholder until DOMPurify is wired in.
