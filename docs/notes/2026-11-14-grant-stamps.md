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
destination unconditionally, since its path isn't known beforehand; only
that restamp takes the path's turn and is marked in flight, so a reader
between the copy's write and its mark can drop the grant, which fails safe.
Rooms, REST, MCP and run copies all hold that wrapper. A rename keeps the
inode and modification time, so it needs no new stamp; `Collab::rename`
checks the old path's grant before renaming (a file replaced outside and
then renamed doesn't carry its grant to the new name) and restamps the new
path after the ids follow, with the new path marked in flight meanwhile, so
a filesystem that differs is covered.

The race that matters is a grant read between a write and its restamp, which
would drop the grant of a file the server just saved. A check keeps the
grant, with no stamp to compare, when a save of that path is in flight or
any save began or ended while it looked at the file (each path keeps its
in-flight count and the sequence number of its last begin or end, compared
before and after the `stat`, so a save of another path doesn't void the
check; a path forgotten to keep the table small voids the checks that began
before it). That covers readers in this process. Another process (`ganymede
grant --list`, a second server) checking in that window sees the new file
and the old stamp and drops the grant: it fails safe, as does an older
build's save. There is a known, tiny window the other way: an outside
rewrite that lands between the server's write and its restamp is blessed by
the restamp, since the stamp is taken from the file as it is then.

`ganymede grant` stores the stamp and reads through the checked forms, with
the `stat` inside the store's lock. A program that writes a notebook without
the server (`ganymede execute`, `ganymede nb clear`) doesn't know which root
its grant was made under: it may run from a subdirectory, or on a notebook
outside the cwd. Their one writer, `write_notebook`, and the server's saves
use the same helper in `ganymede-contents` (`held`, `Held`): it finds every
root that has a file id for the file (one query of the shared database by
each candidate (root, relative path) from the file's ancestor directories,
`roots_with_id`), checks each root's grant just before writing, and restamps
with the same compare-and-swap after. It doesn't go through the host API,
with or without one. Its write is not marked in flight, so another process
reading the grant in that window drops it, which fails safe. Windows has no
inode in the stamp, so a same-size replacement by rename that keeps the
modification time isn't seen there.

Grants are per (root, path) in the shared database, so a server on `/` and
`ganymede serve` or `ganymede grant` in a project directory each hold their
own grant on one file, and a save by one must not drop the other's. The
server's saves (`save`, `restore_checkpoint`, `copy`) look for the same
file's grants under every other root (`Collab::check_others`), check them
before the write and restamp them after, with the same tokens; with ids in
memory there is only the own root. The in-flight marks and turns are this
process's, so another process reading one of those other grants between the
write and its restamp drops it, which fails safe. A rename moves its own
root's ids and those of every other root above both paths that has an id
under the old one (`FileIds::moved_in_other_roots`, a file or a
directory), so their grants follow; a rename out of such a root leaves that
root's ids as they were. The stamp doesn't depend on the path, so they need
no restamp. A rename takes the write side of a lock that every save, restore
and copy holds for reading, after the move lock, so it waits for the saves in
flight under either path, files or a directory's, and none begins meanwhile;
a save in flight when it lands finishes and restamps first.

The server's saves reuse the connection their `FileIds` already has for the
`roots_with_id` query (`held_with`). The query joins `host_grants`, so it
returns only roots that hold a grant on the file, and `held_with` opens
another root's `FileIds` only for those. `moved_in_other_roots` has its own
query, since it moves every root's ids, grant or not.

A delete through the server drops the ids, and so the grants, that every
other root above the path holds on it (`FileIds::removed_in_other_roots`),
so a file made anew at the path inherits none. A save that fails after
writing still restamps, since the check's compare-and-swap blesses nothing
that changed outside. `copy` runs detached, as a save does. A grant look
that a cleanup of the remembered paths overlapped is made again, up to
three times, not given up as unstable, so a quiet path's save keeps its
restamp. The database check gives a row that changed under its compare the
verdict once more before returning it.

Known limit, left for a follow-up: a rename holds the move lock for writing
while it waits for the write side of the tree lock, so the server's room
opens and deletes pause for as long as the slowest save in flight. A
per-path or per-prefix wait would avoid that.
