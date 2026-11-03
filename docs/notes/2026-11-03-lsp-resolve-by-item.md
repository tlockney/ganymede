## 2026-11-03, LSP resolves go by their item's document (#339)

The LSP relay resynced a `*/resolve` against the document of the
client's latest request of the matching kind, which isn't the one that
returned the item when a tab completes in one document and then
another (one connection serves every notebook in a browser tab).

The router now remembers the document a `textDocument/completion`,
`codeAction`, `codeLens`, `inlayHint` or `documentLink` request named,
and wraps each item's `data` in the answer as
`{"ganymede.uri": <uri>, "data": <server's data>}`. A client sends the
item back unchanged when it resolves it, so `Documents::before_request`
reads the document from the mark, and the router puts the server's own
`data` back before the request goes on. An item without `data` takes a
completion list's `itemDefaults.data`, since the wrapped item no longer
inherits it. `Command`s in a code action list are left unmarked. An
item that comes back unmarked falls back to the client's last request
of that kind, as before.
