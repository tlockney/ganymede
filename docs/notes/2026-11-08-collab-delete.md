## 2026-11-08, Collab::delete and the session route's hidden-file error (#580)

Follow-ups from the #575 review. `DELETE /api/contents` took
`begin_move` inline, and the test that covered it took the lock itself,
so nothing showed the route did. The delete now lives in
`Collab::delete`, which the route calls and a test drives: it waits for
an index in progress and leaves the path without an id.

`PUT /api/collaboration/session` turned every 404 from the contents
check into "does not exist"; a hidden file now keeps its own reason.
`index_existing` says why the SQLite insert stays under the read guard
(a busy shared database can hold back a rename), and the
`index-existing` note quotes the MCP error as it reads.
