## 2026-11-06, room lock retry within one server (#400)

A room's lock is a local of its task, so it drops only when the task
returns, a few statements after `retire` takes the room off the map. A
room opening on the same file in that gap found the path in this
server's held set and opened writable with no lock, and the tick retried
the lock only while read-only, so it never took it. Once the old task
ended, another server's room could take the file and both autosaved.

`Locks::acquire` now tells that case apart (`HeldHere`), and `take_lock`
returns whether to try again, so the tick retries while read-only or
while another room of this server holds the lock.
