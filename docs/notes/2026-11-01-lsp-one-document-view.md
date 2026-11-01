## 2026-11-01, One copy of each document for a shared language server (#262)

- Two tabs on one notebook each send `didOpen` and each send every
  edit. Relayed as they were, the language server opened the document
  twice and applied each edit twice, and so did the `.virtual_documents`
  shadow file. `jupyter-lsp` behaves the same.
- Per-client URIs (`.virtual_documents/<client>/...`) would have fixed
  it, but they need rewriting URIs both ways, including in diagnostics
  sent to every client, and would run a server's analysis once per tab.
  Instead the relay keeps each client's text and one server copy per
  URI (`documents.rs`): a second `didOpen` or a duplicate `didChange` is
  dropped when the server's text already matches, an edit made against
  an older text is sent as the full text, versions are the relay's, and
  `didClose` goes out when the last client closes or disconnects.
- The shadow file is now the server's copy, written whole, instead of
  each message's first change applied to whatever was on disk. The
  document lock is held across the disk write and the send, so the disk
  and the server see changes in the same order.
- Still shared: request ids. Each client numbers its own requests and
  every response goes to every client, so two tabs' ids collide (#269).
