## 2026-11-15, creates no longer hold up grant reads (#829)

A copy into a directory and a new untitled file held the move lock for
writing for the whole create, because the new file's path isn't known
until the create returns, and a stale id or grant left at that path by a
file deleted outside must be forgotten before anything reads it. Grant
reads, path-and-grant reads, indexing and room opens take the move lock for
reading, so all of them waited until a large copy finished, whatever path
they were after.

A create now takes the move lock for reading and claims its directory (for a
copy with no directory given, the directory of its source) until the new
path is forgotten. A grant read, an index and a room open register the
path they look at, and wait only while a create claims a directory above
it; a create waits for the lookups already registered under its
directory. A lookup by file id, which only learns the path after reading it,
registers it and reads it again, and starts over if it changed. So lookups
of paths elsewhere no longer wait for a create, and a lookup under the
directory still can't see the stale grant or id, or open a room on the new
file under the stale id. A create in the root claims the whole tree, as
every path is under the root, so lookups anywhere wait for it.

Renames, deletes, and the check that finds a file deleted outside still take the move lock
for writing, so they wait for a create in progress and none lands during
one. Two creates in one directory no longer take turns on the lock; the
contents reserve each name with an exclusive create, so they get different
files.

The registry the writes and renames of #830 used is now a small type of its
own, `PathLocks`, with one instance for writes against renames and one for
lookups against creates. The lock order is the move lock, then the lookups,
then the writes, then a path's turn.

This closes #829.
