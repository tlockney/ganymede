## 2026-11-08, room lock probe vs. this server's own room (#547)

`Locks::owned_elsewhere` takes and drops a notebook's room lock to answer
the contents API's `writable`. flock locks belong to the open file, not
the process, so a room of the same server calling `acquire` during the
probe got `WouldBlock` and started read-only, as if another server owned
the file, until its next retry. Both now hold the server's `held` set
across their `try_lock`, so they can't overlap. The test also caught a
second gap: `RoomLock::drop` left the held set before its file closed, so
a probe in between found the lock taken and not held here. It now closes
the file first, under the same guard. The new test runs a prober thread
against 2000 acquires. Undoing the `acquire` fix fails it every run; undoing
only the `Drop` fix fails it some runs, since that window is narrower.
