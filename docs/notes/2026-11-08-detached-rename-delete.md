## 2026-11-08, rename and delete finish when the client leaves (#586)

axum drops a handler's future when its client disconnects. A rename or
delete dropped after the disk changed but before `Collab::moved` or
`removed` left the file-id table, and any open room, on the old path.
`Collab::rename` and `Collab::delete` now run the lock, the disk change
and the id update on their own task and wait for it, so dropping the
request no longer stops them. The REST handlers call them. A test aborts
the caller while the operation waits for the lock and checks the file
and its id still move or go.
