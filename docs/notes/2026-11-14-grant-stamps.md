## 2026-11-14, Drop a host grant when its file changes outside Ganymede (#767)

A notebook's host grant is keyed by file id, and a file replaced outside the
server (`git checkout`, Finder, a download, a sync tool) keeps its path's
id, so the new content inherited the old grant. Hashing the file at every
grant read wasn't wanted, so each grant now carries a stamp of its file:
size, modification time in nanoseconds and, on Unix, inode number (not the
device, which isn't stable across remounts), from one `stat`
(`ganymede_contents::file_stamp`). The change time was tried first and
dropped: on macOS it moves with `chmod`, extended attributes (Finder,
quarantine, iCloud) and links, so grants would go with no edit. An atomic
replace (`git checkout`, most editors, downloads) makes a new inode and an
in-place rewrite changes the modification time; a same-size in-place rewrite
that restores the old modification time is not seen, and neither is one
within a tick of a filesystem with coarse timestamps (exFAT, HFS+). Windows
has size and modification time only. `host_grants` gets a nullable `stamp`
column, added when a database lacks it (several servers share one, so a
"duplicate column" from a concurrent add is fine).

`FileIds` stays pure: callers pass the stamp. `grant_checked(id, current)`
keeps a grant whose stamp matches or whose file can't be stat'ed, stamps
one that has none (trust on first use, so grants from before this change
aren't dropped), and otherwise deletes it and logs the path.
`grants_checked` does the same for the listing, so `ganymede grant --list`
shows only grants that hold. In SQLite the stored stamp is tagged with
a hash (FNV-1a, written out, since `DefaultHasher` varies by Rust version)
of the grant it was taken for: an older build sharing the database can
change a grant without its stamp, and a stamp for another grant counts as
none (trust on first use). If the file changed and the database won't
delete the grant, it isn't handed out. `restamp` updates a stamp only
where a grant row exists.

Saves must restamp, and patching every call site would miss the next one,
so `Collab::stamping` wraps `AppState.contents`: `save` and
`restore_checkpoint` check the grant first (so a write doesn't bless a
file that changed outside), mark the path in flight, write, restamp, and
unmark, all on a detached task so a dropped request can't leave a write
unstamped. `copy` restamps its destination. Rooms, REST, MCP and run
copies all hold that wrapper. A rename keeps the inode and
modification time, so it needs no new stamp; `Collab::rename` still
restamps the new path after the ids follow, with the new path marked in
flight meanwhile, so a filesystem that differs is covered.

The race that matters is a grant read between a write and its restamp,
which would drop the grant of a file the server just saved. A check keeps
the grant, with no stamp to compare, when a save of that path is in flight
or any save began or ended while it looked at the file (a counter bumped at
both ends, checked before and after the `stat`). There is a known, tiny
window the other way: an outside rewrite that lands between the server's
write and its restamp is blessed by the restamp, since the stamp is taken
from the file as it is then.

`ganymede grant` stores the stamp and lists through the checked reads.
`ganymede execute` writes the notebook itself, so it checks the grant
before the run (otherwise its write would restamp a replaced file) and
restamps after writing, through the host when there is one and through
the file ids directly when the config didn't load. Its write is not
marked in flight, so another process reading the grant in that window
drops it, which is the safe direction. Also not marked: a `copy` over an
existing file by name (it restamps afterward). `Collab::rename` checks
the old path's grant before renaming, so a file replaced outside and then
renamed through the server doesn't carry its grant to the new name.

The stamp is taken with the store held (`grant_checked_with`,
`set_grant_with`, `restamp_with` take a closure), not before it: taken
first, a whole save and restamp could land between the `stat` and the
comparison and the check would drop a valid grant, or a grant set could
overwrite a fresher restamp. A check now sees either the restamp or the
save in flight, and two concurrent saves leave the later stamp.

A program that writes a notebook doesn't know which root its grant was made
under: `ganymede execute` from a subdirectory, or on a notebook outside the
cwd, has the grant under another root. `write_notebook`, used by `execute`
and `nb clear`, checks before writing and restamps after under every root
that has a file id for the file: one query of the shared database by each
candidate (root, relative path) from the file's ancestor directories
(`roots_with_id`). This replaces the host's own check and restamp.
