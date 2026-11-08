## 2026-11-08, Rooms: no room opens mid-rename (#522)

`patch_contents` renamed the file on disk, then `Collab::moved` moved the
file ids and bumped `moves`. A join in between read the new `moves`, looked
the id up (still the old path), took the rooms lock before `moved` did, and
opened the room on a file that was gone: the client got 4404. Checking
`moves` can't catch that, since nothing had bumped it yet. `Collab` now has
a `move_lock` (a tokio `RwLock<()>`): the rename holds it for writing,
through `Collab::begin_move()`, from before the disk change until the ids
follow, and opening a room holds it for reading across the path lookup. A
joiner waits for a rename without polling, and a failed rename just drops
the lock, since the path hasn't changed. `moves` still catches a rename or
delete that finishes between the lookup and the open.

The websocket join and `send_to_notebook_room` now share one helper,
`send_to_room`, and `serve_socket` no longer looks the path up first. An
open room takes the join before any lookup, so a rename elsewhere costs a
joiner nothing, and a client joining the room of a just-deleted file
reaches the room (which says "File deleted") instead of getting "Unknown
file id". A room entry whose task has stopped is replaced by a fresh room
rather than reported as an unknown file id.
