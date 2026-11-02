## 2026-11-02, Language server line ends and position encoding (#276)

The relay's copy of each open document applies clients' ranges itself, and
sends its text in full when a client's edit no longer fits. So its position
arithmetic has to match the server's, or a resync overwrites the server's
correct copy with a wrong one.

- LSP ends lines at `\r\n`, `\r` and `\n`. The copy counted `\n` only, so
  in a document with a lone `\r` every range after it landed a line late.
- `character` counts the encoding the server chose in its `initialize`
  result (`capabilities.positionEncoding`), UTF-16 when it names none. The
  router already sees that response, since it maps each response back to
  its client, so it notes the choice and the document copy reads it from
  there. Each new server process starts back at UTF-16.
- A server picks UTF-8 or UTF-32 only when the client offered it in
  `general.positionEncodings`. The latest `initialize` answer wins, so
  every client of one process is counted the same way.
- Every client's `initialize` reaches the one server process, and a server
  may answer the second with an error. Only a `result` changes the
  encoding. The document copy isn't rebuilt when the encoding changes, so
  text applied under one encoding would be read under another; that takes
  a second successful `initialize` choosing differently, which a
  conforming server doesn't do.
