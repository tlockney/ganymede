## 2026-11-08, Rooms: no room opens mid-rename (#522)

`patch_contents` renamed the file on disk, then `Collab::moved` moved the
file ids and bumped `moves`. A join in between read the new `moves`, looked
the id up (still the old path), took the rooms lock before `moved` did, and
opened the room on a file that was gone: the client got 4404. Checking
`moves` can't catch that, since nothing had bumped it yet. The rename now
takes `Collab::begin_move()` before touching the disk: a guard that counts
renames under way and bumps `moves` on the way in and out, and no room
opens while one is under way (the joiner sleeps 5 ms and looks again). A
failed rename drops the guard too.

The websocket join and `send_to_notebook_room` now share one helper,
`send_to_room`. It sends to an open room before any path lookup, so a
rename elsewhere costs a joiner nothing, and a client joining the room of a
just-deleted file reaches the room (which says "File deleted") instead of
getting "Unknown file id".
