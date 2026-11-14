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

`FileIds` stays pure: callers pass the stamp, or a closure that takes it
while the store is held (`grant_checked_with`, `set_grant_with`,
`restamp_if`). Taken before the lock, a whole save and restamp could land
between the `stat` and the comparison and the check would drop a valid
grant, or a grant set could overwrite a fresher restamp; taken inside, a
check sees either the restamp or the save in flight. `grant_checked` keeps a
grant whose stamp matches or whose file can't be stat'ed, stamps one that
has none (trust on first use, so grants from before this change aren't
dropped), and otherwise deletes it and logs the path. `grants_checked` does
the same for the listing, so `ganymede grant --list` shows only grants that
hold. In SQLite the stored stamp is tagged with a hash (FNV-1a, written out,
since `DefaultHasher` varies by Rust version) of the grant it was taken for:
an older build sharing the database can change a grant without its stamp,
and a stamp for another grant counts as none (trust on first use). An older
build also saves without restamping, so a new build drops the grant after an
old build's save; that fails safe. If the file changed and the database
won't delete the grant, it isn't handed out and nothing restamps it.

Saves must restamp, and patching every call site would miss the next one, so
`Collab::stamping` wraps `AppState.contents`. `save` and
`restore_checkpoint` take their path's turn (one server write of a path at a
time), run `check_for_write` (which drops a grant the file outgrew, so the
write doesn't bless it, and returns the stamp the grant is stored under, or
nothing when no grant stands), mark the path in flight, write, and restamp
only if the grant is still stored under that stamp (`restamp_if`, a
compare-and-swap), then unmark. All of that runs on a detached task so a
dropped request can't leave a write unstamped. `copy` restamps its
destination unconditionally, since its path isn't known beforehand. Rooms,
REST, MCP and run copies all hold that wrapper. A rename keeps the inode and
modification time, so it needs no new stamp; `Collab::rename` checks the old
path's grant before renaming (a file replaced outside and then renamed
doesn't carry its grant to the new name) and restamps the new path after the
ids follow, with the new path marked in flight meanwhile, so a filesystem
that differs is covered.

The race that matters is a grant read between a write and its restamp, which
would drop the grant of a file the server just saved. A check keeps the
grant, with no stamp to compare, when a save of that path is in flight or
any save began or ended while it looked at the file (a counter bumped at
both ends, checked before and after the `stat`). That covers readers in this
process. Another process (`ganymede grant --list`, a second server) checking
in that window sees the new file and the old stamp and drops the grant: it
fails safe, as does an older build's save. There is a known, tiny window the
other way: an outside rewrite that lands between the server's write and its
restamp is blessed by the restamp, since the stamp is taken from the file as
it is then.

`ganymede grant` stores the stamp and reads through the checked forms, with
the `stat` inside the store's lock. A program that writes a notebook without
the server (`ganymede execute`, `ganymede nb clear`) doesn't know which root
its grant was made under: it may run from a subdirectory, or on a notebook
outside the cwd. Their one writer, `write_notebook`, finds every root that
has a file id for the file (one query of the shared database by each
candidate (root, relative path) from the file's ancestor directories,
`roots_with_id`), runs `check_for_write` under each just before writing, and
restamps with the same compare-and-swap after. It doesn't go through the
host API, with or without one. Its write is not marked in flight, so another
process reading the grant in that window drops it, which fails safe. Windows
has no inode in the stamp, so a same-size replacement by rename that keeps
the modification time isn't seen there.
