## 2026-11-03, LSP resync before resolve requests (#334)

#330 gave the server the requester's text before a request that names
a `textDocument`. Follow-ups such as `completionItem/resolve` carry only
the item the server returned, so they went against whichever tab's
text the server had. jupyterlab-lsp sends a resolve for the selected
completion item, and the server's `additionalTextEdits` (auto-imports)
come back with positions in that text.

The relay now remembers, per client, the document its last
`textDocument/*` request named, and treats the `*/resolve` methods as
requests about that document. Call and type hierarchy requests carry
their document in `params.item.uri`, so they use that instead. The
resolve methods don't echo the URI reliably (`data` is the server's own
field, free-form), so the relay doesn't read it.

A client's last named document is forgotten when it disconnects. The
unparsed-message filter in `LspClient::send` now also passes these
methods, so other messages still skip the JSON parse and the document
lock.
