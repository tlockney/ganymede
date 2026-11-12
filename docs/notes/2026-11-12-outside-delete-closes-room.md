## 2026-11-12, an outside delete closes the room (#719)

A notebook deleted by another program (`rm`, not `DELETE /api/contents`)
left its document room open. The watcher's reload logged `cannot reload`
and kept the old cells, and the path kept its file id. A new file made
later at that path got the same id, so a client joined the stale room
and saw the deleted notebook. ganymede-ui's e2e suite found it (#510).

Now `reload` and `poll` report a missing file as gone. The room then takes
`Collab::begin_move`'s lock, as an API delete does, checks the file is
still gone, and calls `Collab::removed`: the ids go, and the room closes
its clients with 4404 through the same `Cmd::Removed` an API delete
sends. A file recreated before the check is merged as an ordinary change.
The room skips its save in that tick, since a save would write the
deleted file back; a save that meets the delete (412, then a reload that
finds no file) stops retrying.

The test in `tests/http.rs` deletes the file from the filesystem, waits
for the 4404, writes a new file at the path, and checks it gets a new id
and opens with its own cell.
