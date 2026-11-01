## 2026-10-31, Language servers for jupyterlab-lsp (#249)

The spike (#236, its own entry on this page) found that `jupyter-lsp`'s server side is a language-free relay, so this builds it: a `ganymede-lsp` crate, `GET /lsp/status` and `/lsp/ws/{name}` in `ganymede-server`, `language_servers` tables in `ganymede.toml`, and `virtualDocumentsUri` in the lab page config.

The config key is `language_servers` (`[language_servers.<name>]`), after `jupyter-lsp`'s `LanguageServerManager.language_servers`, rather than the `[lsp.servers.<name>]` #249 sketched, so a spec copied from a Jupyter config keeps its name. Servers come only from the config file, in `jupyter-lsp`'s spec format; unknown spec keys pass through to the frontend. No spec finders, so the host still names no language. A spec with an empty `argv` is skipped, as in `jupyter-lsp`, and with no servers configured the routes answer 404, as a server without `jupyter-lsp` does.

What differs from `jupyter-lsp`, on purpose:

- Range edits to virtual documents count UTF-16 code units, as LSP and the browser do. `jupyter-lsp` counts Python code points, which is wrong after an astral character. The document is also kept byte for byte rather than split into lines and rejoined, so a trailing newline survives.
- A server that exits closes its clients' websockets and is started again by the next client. `jupyter-lsp` leaves the session pointing at the dead process.
- Servers start in the root dir, not the server's working directory.
- A server runs in its own process group on Unix, and stopping it signals the group (SIGTERM, then SIGKILL after 2 s), so workers it started (`tsserver`, or the real server behind an npm shim) don't outlive it. On Windows a bare name is looked up through `PATHEXT`, as `shutil.which` does, so npm's `.cmd` shims start.
- Shadow writes are serialized, so two clients' edits can't overwrite each other. Two tabs on one notebook still both send each incremental edit, which is then applied twice; `jupyter-lsp` has the same problem (#262).
- `rootUri` and `virtualDocumentsUri` are percent-encoded, as `Path.as_uri()` does, so a root with a space (iCloud's `Mobile Documents`) or `#` gives a valid URI, and a Windows verbatim path from `canonicalize` (`\\?\C:\...`) becomes `file:///C:/...`. The old `rootUri` had both problems.
- Each client gets an unbounded queue rather than a lossy broadcast channel: a dropped JSON-RPC response would leave the editor waiting forever.

Tested with `cat` as the language server: it echoes each framed message, so a client's message comes back through the real stdio path to every client. That covers framing, the shared process, stop on the last close, restart after exit, `env` expansion, and shadow writes without a real language server in CI. By hand: JupyterLab 4.6.5 with `jupyterlab-lsp` 5.3.0 (installed with `ganymede lab extension install`) against `ganymede serve`, a `.ts` file with `typescript-language-server` and a Python notebook with `jedi-language-server`. Completion, hover, diagnostics, and the `.virtual_documents/py.ipynb` shadow file all worked, and `/lsp/status` showed the server stopped after the page closed.

Lesson: the websocket route can't be checked with a plain GET in the router tests. axum's `WebSocketUpgrade` extractor answers 400 before the handler runs, so a 404 for an unconfigured server shows only on a real websocket handshake.
