## 2026-11-08, file ids only for paths that still exist (#574)

`notebook_room_id` and the REST `session` and `fileid/index` routes
checked a path existed, then indexed it, with nothing stopping a rename
or delete in between. `Collab::index_existing` does both under
`move_lock` for reading, and `DELETE /api/contents` now takes the lock
for writing, as renames did (#522), so a path that's gone never gets an
id. `notebook_room_id` returns the contents error, so MCP and the
execute endpoint say why a notebook can't be opened ("cannot open x:
no such file or directory: x") instead of only "cannot open x".
