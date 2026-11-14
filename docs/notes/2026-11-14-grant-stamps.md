## 2026-11-14, Drop a host grant when its file changes outside Ganymede (#767)

A notebook's host grant is keyed by file id, and a file replaced outside
the server (`git checkout`, Finder, a download, a sync tool) keeps its
path's id, so the new content inherited the old grant. Hashing the file at
every grant read wasn't wanted, so each grant now carries a stamp of its
file: size, modification time and, on Unix, change time in nanoseconds,
from one `stat` (`ganymede_contents::file_stamp`). The change time moves
with every write and can't be set back, as the modification time can by
`cp -p` or `touch -r`, so it catches a rewrite that keeps the other two;
Windows has size and modification time only. `host_grants` gets a
nullable `stamp` column, added when a database lacks it (several servers
share one, so a "duplicate column" from a concurrent add is fine).

`FileIds` stays pure: callers pass the stamp. `grant_checked(id, current)`
keeps a grant whose stamp matches or whose file can't be stat'ed, stamps
one that has none (trust on first use, so grants from before this change
aren't dropped), and otherwise deletes it and logs the path.
`grants_checked` does the same for the listing, so `ganymede grant --list`
shows only grants that hold. `restamp` updates a stamp only where a grant
row exists.

Saves must restamp, and patching every call site would miss the next one,
so `Collab::stamping` wraps `AppState.contents`: `save` and
`restore_checkpoint` check the grant first (so a write doesn't bless a
file that changed outside), mark the path in flight, write, restamp, and
unmark, all on a detached task so a dropped request can't leave a write
unstamped. `copy` restamps its destination. Rooms, REST, MCP and run
copies all hold that wrapper. A rename changes the file's change time on
Linux, so `Collab::rename` restamps the new path after the ids follow, with
the new path marked in flight meanwhile.

The race that matters is a grant read between a write and its restamp,
which would drop the grant of a file the server just saved. A check keeps
the grant, with no stamp to compare, when a save of that path is in flight
or any save began or ended while it looked at the file (a counter bumped at
both ends, checked before and after the `stat`). It errs toward keeping:
an outside rewrite that lands during a save is caught on the next read.

`ganymede grant` stores the stamp and lists through the checked reads.
`ganymede execute` writes the notebook itself, so it checks the grant
before the run (otherwise its write would restamp a replaced file) and
restamps after writing. Not marked in flight: a `copy` over an existing file by name (it restamps
afterward), and the CLI's own writes other than `execute`'s.
