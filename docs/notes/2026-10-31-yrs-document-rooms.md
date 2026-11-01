## 2026-10-31, yrs document rooms (#243)

Spike for roadmap step 6 (document model, #141) with `yrs` as the vehicle,
which #244 (JSD execute) and #245 (`ganymede mcp`) build on. Result: it
works. A new crate, `ganymede-documents`, holds a notebook as a `yrs`
document in `jupyter_ydoc`'s schema, and `ganymede serve` answers
`jupyter-collaboration`'s session endpoint and room websocket. JupyterLab
4.6.4 with `jupyter-docprovider` and `jupyter-collaboration-ui` 3.0.4
(installed with `ganymede lab extension install`) opened a notebook through
a room; in Playwright, text typed in one tab showed up in a second, was
autosaved, and a change written to the file by another process appeared in
both tabs.

### Findings

- **Schema.** Read from `jupyter_ydoc` 4.2.0 and `@jupyter/ydoc` as shipped
  in `jupyter-docprovider` 3.0.4: root `cells` (array of maps: `source` a
  Text, `metadata` a Map, `outputs` an array of maps with a stream's `text`
  a Text, `execution_state` added to code cells), `meta` (`nbformat`,
  `nbformat_minor`, `metadata` as a Map), and `state` (`dirty`, `path`,
  `hash`; clients write `document_id`). JupyterLab learns of a save from a
  change to `state.hash`.
- **Byte-identical round trip.** Every nbformat fixture goes through a room,
  and through a client synced from it, and comes back byte for byte. Two
  things needed care. `jupyter_ydoc` adds `language_info` and `kernelspec`
  defaults and turns every top-level number into a float; this crate does
  neither. And Yjs can't hold every JSON number: `lib0` writes integers
  outside 32 bits as floats, and nothing holds integers beyond 2^53. Rooms
  keep the notebook as last loaded and put such numbers back on save when
  the value in the document is what the original turns into
  (`restore_numbers`), so a file with `18446744073709551616` in its
  metadata still saves unchanged.
- **Deterministic load**, as `jupyter-collaboration` does: the initial
  content is built in a document with client id 0 and keys in sorted
  order, so a room rebuilt from an unchanged file has the same history and
  a client that reconnects merges without duplicating cells.
- **Wire protocol.** y-websocket sync (step 1, step 2, update) and
  awareness, which `yrs::sync` encodes. `jupyter-collaboration` reuses
  message type 2, y-protocols' auth, for raw string messages: the client
  sends `save` plus a request id on Ctrl-S and waits for
  `{"type": "save", "responseTo", "status"}`. The room reads type 2 itself
  before handing anything to `yrs`. A stale `sessionId` (after a server
  restart) is closed with 1003 and a JSON reason, which makes JupyterLab
  offer to reload.
- **Dirty tracking.** A first version counted any update as an edit, and
  each tab opening the notebook caused a save, because clients write
  `state.document_id`. Rooms now watch `cells` and `meta` with deep
  observers and ignore `state`.
- **Reload** is granular, like `jupyter_ydoc` 4's `set`: cells with the
  same id and content stay, a cell whose source alone changed gets one
  text edit (common prefix and suffix kept, so other people's cursors
  stay put), the rest are replaced. Changing one line of a cell on disk
  sends about 100 bytes.
- **Room shape.** `Room` is I/O-free (runt's pattern, #141): the host feeds
  it frames and connection events and gets back `Effects` (frames for one
  client, all but the sender, or all; save requests; whether content
  changed). `generation()` and `saved(generation, hash)` let the host save
  in the background without losing edits made during the write. The same
  code can run in the apps through `ganymede-ffi`.
- **Server driver** (`ganymede-server/src/collab.rs`): one task per room.
  It loads through `Contents`, autosaves a second after the last edit with
  an `If-Match` on the last hash, and polls the file every second (cheap
  `last_modified` check, then a hash, so its own saves are skipped). A
  save refused with 412 reloads from disk, as `jupyter-collaboration`'s
  out-of-band handling does: the disk wins. Rooms without clients are
  saved and dropped after a minute.
- **Licenses.** `yrs` is MIT; its new dependencies (`arc-swap`,
  `async-lock`, `dashmap`, `event-listener`, `parking`, `smallstr`) are
  MIT or Apache-2.0, all in `about.toml`.

### Plan for the real implementation

1. **File ids** that persist and follow renames (jupyter_server_fileid
   keeps them in SQLite; `ganymede-contents` already has SQLite for trust),
   and `PATCH` renames moving open rooms (`Room::set_path`).
2. **Watcher** per #141 instead of polling: watch the parent directory,
   debounce, skip our own writes by hash. Keep polling as the fallback for
   file systems without events.
3. **Text files** (`jupyter_ydoc`'s YUnicode: a `source` Text), since
   JupyterLab with the extension opens every file through a room and a
   text file fails to load today.
4. **Kernel outputs into the room** (#244): reuse `ganymede-exec`'s
   `NotebookRun` to apply iopub messages to cells through `Room::edit`,
   streams appended to the output's Text, so outputs land with no client
   connected. Then JSD's `POST /api/kernels/{id}/execute`.
5. **Divergent saves:** today the disk wins and unsaved room edits are
   lost when another program writes the file in the same second. Merge
   instead: reload into the room as a granular update on top of local
   edits, and refuse only when the same cell changed on both sides.
6. **One owner per file** across processes (Mac server and iPad app on
   one iCloud file), the lease from runt's ADR.
7. **`ganymede-ffi`** entry points for rooms, for the apps.
8. A JupyterLab end-to-end test in `e2e/` with the two extensions, like
   the Playwright run above.
