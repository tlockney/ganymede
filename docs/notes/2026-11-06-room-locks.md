## 2026-11-06, one owner per notebook room on a machine (#272)

Two `ganymede serve` processes with a room open on one notebook both
autosaved it and each merged the other's saves as outside changes. Now
the first room takes an exclusive lock, an empty file in the runtime
dir's `ganymede-rooms/` named for the sha256 of the notebook's canonical
path, through `std::fs::File::try_lock` (`flock`/`LockFileEx`, std since
1.89, so no new dependency). A room that can't take it is read-only.

- `Room::set_read_only` lives in ganymede-documents: a client's update
  that changes content is applied, then the content is set back to the
  baseline (the file as last read) and the result goes to every client,
  so the sender's edit visibly disappears. Server edits (outputs) don't
  make it dirty. Refusing the update without applying it would leave the
  sender's document out of step with the room for good.
- `save_room` returns `Skipped` for a read-only room, including a
  client's forced `save`.
- The contents API reports `writable: false` for a notebook another
  process's room owns. `Locks::owned_elsewhere` finds out by taking and
  dropping the lock; a lock this server holds is tracked in a set, since
  a second `flock` on the same file from the same process fails too.
- A read-only room retries the lock every second, so it takes over once
  the owner's room closes or its process dies (the OS drops the lock).
- Lock files are never deleted: deleting one while another server is
  about to lock it would let two servers own the file.
- Embedding apps (`Startup::stop` set) don't lock. Across devices there
  is no lease, as Thomas decided; a proxy to the owner can come later.
