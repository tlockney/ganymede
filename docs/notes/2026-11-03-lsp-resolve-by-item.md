## 2026-11-03, LSP resolves go by their item's document (#339)

The LSP relay resynced a `*/resolve` against the document of the
client's latest request of the matching kind, which isn't the one that
returned the item when a tab completes in one document and then
another (one connection serves every notebook in a browser tab).

The router now numbers the document a `textDocument/completion`,
`codeAction`, `codeLens`, `inlayHint` or `documentLink` request named,
and wraps each item's `data` in the answer as
`{"ganymede.doc": <n>, "data": <server's data>}`. A number, not the URI,
because a completion list can hold thousands of items. A client sends
the item back unchanged when it resolves it; the session reads the
document from the mark before the router takes it off and puts the
server's own `data` back. A resolve's answer is marked too, so a
resolved item resolved again keeps its document. A completion list's
`itemDefaults.data` is marked in its place, and items without `data`
are left to inherit it, so the default isn't copied into each item. `Command`s in a code action list are left
unmarked, and `data` the router didn't mark passes through as it is.
An item that comes back unmarked falls back to the client's last
request of that kind, as before.
