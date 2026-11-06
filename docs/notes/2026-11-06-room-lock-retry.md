## 2026-11-06, room lock released on retire (#400)

A room's lock was a local of its task, so it dropped only when the task
returned, a few statements after `retire` took the room off the map. A
room opening on the same file in that gap found the path in this
server's held set and opened writable with no lock. The tick retried the
lock only while read-only, so it never took it, and once the old task
ended another server's room could take the file and both autosaved.

`retire` now drops the lock while it still holds the `rooms` mutex. A
room for the file can only be created after that, so it finds the lock
free (or owned by another server) rather than held here. Retrying on the
tick alone would have left a gap of up to a second and a quarter in which
another server could take the lock from under a room with client edits.

`Locks::acquire` still reports a lock held by another room of this
server (`HeldHere`), which now means two file ids for one file (a
symlinked directory, a hard link, a case-insensitive name). That room
stays writable and retries each second; both rooms autosave, as before.
A read-only room that gets `Unavailable` (the file missing for a moment
while another program replaces it) now keeps retrying too.
