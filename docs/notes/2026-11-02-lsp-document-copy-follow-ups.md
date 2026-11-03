## 2026-11-02, Language server document copy follow-ups (#277)

Follow-ups to the relay's one-copy-per-document table (#262):

- A `didChange` that leaves the sending client's own text unchanged (an
  empty `contentChanges`, say) is dropped. Before, a tab that was behind
  sent its older text in full and the server flipped to it.
- When the client whose text the server has leaves, the server gets the
  text of the remaining client that opened or changed the document last,
  as the docs said, rather than the lowest client id. A per-table clock
  orders opens and changes.
- A change whose `range` is present but not a range is skipped instead of
  being applied as a full replacement, and the server then gets the full
  text the relay applied, so it can't read that change its own way.
- A client's cleanup on drop runs on the runtime it was created on, so it
  still runs when the client is dropped outside a runtime.
- Tests: a relay test for the generation check in `send` (a client of an ended
  server sends and leaves; the new server's document is untouched), and
  the two-tab test no longer sleeps: each tab's cleanup sends under the
  documents lock, so once the last `didClose` arrives everything the tabs
  sent has. The check in `Drop` has no test: client ids keep counting
  across restarts, so an old client's `disconnect` finds nothing in the
  new table either way.

Left open in #277: mapping the relay's document versions back to each
client's numbering, and resyncing to the requesting tab's text before a
position-carrying request. Both need a look at what jupyterlab-lsp does
with versions and positions first.
