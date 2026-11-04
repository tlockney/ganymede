## 2026-11-04, LSP relay follow-ups from the #351 review (#356)

`LspClient::send` chose its path with `message.contains("textDocument/did")`,
so a request whose text merely mentioned a document notification (an
`executeCommand` argument, a completion's label) skipped the router: no
id of the relay's own, so two tabs' ids could clash, and no unmarking of
a resolve's item. It now parses the message once and routes by its
`method`.

Document numbers in item marks (#339) lived in the router, which was
replaced whenever the server restarted. A completion item from before a
restart, resolved after it through a reconnected client, then named
whichever document took its number since. `Router::restart` now clears
the requests and encoding but keeps the numbers, with a `HashMap` beside
the `Vec` for the lookup.

The review worried that the mark had to be read before the router took
it off. It doesn't: `resync_for_request` reads its own parse of the
message, and the router strips the mark from another one, so the order
of the two doesn't matter. The comment now says so, and a relay test
(`a_resolve_follows_its_items_document_through_the_relay`) runs the whole
path through `LspClient`: a tab completes on A, then B, another tab edits
A, and the resolve of A's item sends this tab's text of A first.

Left open in #356: marking items only for servers that advertise
`resolveProvider`. It saves about 25 bytes per item but needs dynamic
registration followed to stay correct.
