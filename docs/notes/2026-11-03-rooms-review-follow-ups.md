## 2026-11-03, Document rooms: four follow-ups from the #273 review (#279)

Four of the six items in #279; field-by-field cell merging and the nits
are left for a later PR.

- **File ids on SQLite errors.** `FileIds` handed out an unstored uuid
  when the database failed, so `path(id)` found nothing and the room
  websocket closed 4404. `FileIds` now keeps a memory map next to the
  database: ids handed out on an error live there, and lookups, moves
  and deletes go to both. The SQLite insert is `INSERT OR IGNORE` then
  `SELECT`, so two servers indexing the same new path agree on one id
  instead of the second failing on the unique key.
- **Debounce bound.** File events under 500 ms apart reset the debounce,
  so a file rewritten constantly (a log-like notebook an agent appends
  to) never reached the room, and the room held off autosave too. The
  room now reloads once the first unsettled event is 5 s old. Autosave
  still waits for the file to settle, on purpose: writing over a file
  another program is writing would race it. A client's `save` writes at
  once.
- **Lost forced save.** When a client's `save` met a newer file (412),
  merged, and the disk won the cell the client had edited, the reply was
  `success`. It is now `failed`; the merge is still saved. The room
  doesn't track edits per client, so `failed` means a cell the room had
  changed lost, whichever client (or execution) changed it. Only the
  save's own merge counts: a reload by the watcher before the save is
  logged, as before.
- **Deletes reach open rooms.** `Collab::removed` only dropped ids, so a
  room on a deleted file kept its clients and could autosave the file
  back. It now closes their sockets with 4404 and drops the room,
  turning away clients still queued to join. A client that resolved the
  id just before the delete finds the file missing when its new room
  loads, and also gets 4404 (4500 is kept for other load errors). An
  execute batch running in the kernel goes on; its outputs have no room
  to land in, and queued cells answer `unavailable`.
