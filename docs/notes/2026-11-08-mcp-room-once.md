## 2026-11-08, MCP resolves a notebook's room once (#584)

MCP's `room()` looked the path up three times: `notebook_room_id` for
the hold map's key, then `hold_notebook` and `with_notebook` each again,
every time a contents `get` and a file-id lookup under `move_lock`. A
rename between them could put the hold and the call on different rooms.
`hold_notebook` and `with_notebook` now take the room id, so `room()`
resolves once and both reach the same room, which follows its file id
through a rename. A test opens a room by id, renames the file, and reads
the room through the same id.
