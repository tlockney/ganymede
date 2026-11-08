## 2026-11-08, no file id for a path that doesn't exist (#560)

`notebook_room_id` indexed its path in the file-id table before anything
checked the path, so an MCP call with a mistyped path (and the other
server-side room helpers) left a persistent id for a file that doesn't
exist, which `GET /api/fileid/id` then returned. The REST routes already
checked first, as `jupyter_server_fileid` only indexes existing files.
`notebook_room_id` now stats the path through the contents manager and
returns `None` when it is missing; its callers already treated that as
"cannot open".
