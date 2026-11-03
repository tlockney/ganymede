## 2026-11-03, Language server requests follow the requester's text (#277)

The last two items of #277, after #301:

- **Requests against the wrong text.** With two tabs that don't share
  edits, the server has the text of the tab that changed last, so a
  hover or completion from the other tab was answered against text it
  doesn't show. Now a request with an id, a `textDocument/` method and a
  document the requester has open is preceded by the requester's full
  text when the server has another's. The request waits for the
  documents lock and holds it until it is sent, so no other tab's change
  comes between. This resync doesn't count as the tab's own change: when
  a tab leaves, the server still gets the text changed last.
- **Versions.** The relay numbers versions itself, so a version the
  server echoes back doesn't match a client's numbering. jupyterlab-lsp
  5.3.0 never compares them: diagnostics ignore
  `PublishDiagnosticsParams.version` (a comment in
  `features/diagnostics/feature.ts` says so), the edit applicator for
  rename and code actions has only a TODO to check
  `TextDocumentEdit.textDocument.version`, and highlights compare the
  client's own versions before and after its request. So versions stay
  the relay's; mapping them back would be work no client reads.
