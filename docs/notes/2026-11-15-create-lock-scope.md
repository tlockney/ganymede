## 2026-11-15, creates no longer hold up grant reads (#829)

A copy into a directory and a new untitled file held the move lock for
writing for the whole create, because the new file's path isn't known
until the create returns, and a stale id or grant left at that path by a
file deleted outside must be forgotten before anything reads it. Grant
reads, path-and-grant reads, indexing and room opens take the move lock
for reading, so all of them waited until a large copy finished, whatever
path they were after.

A create now takes the move lock for reading and claims the direct
children of its directory (for a copy with no directory given, the
directory of its source) until the new path is forgotten, the root's
included. A new file is one of those children. A new untitled folder is
the exception: the ids and grants dropped for it are all those under its
path, so its create claims the directory's whole subtree. A grant read, an
index and a room open register the path they look at, and wait only while
a create claims the directory that path is directly in; a create waits for
the lookups already registered at paths directly in its directory. A
lookup by file id, which only learns the path after reading it, registers
it and reads it again, and starts over if it changed. That can only
happen if another process sharing the file-id database moved the path, so
no test holds it.

So lookups of paths elsewhere no longer wait for a create, and a lookup
under the directory still can't see the stale grant or id, or open a room
on the new file under the stale id. This holds for the root too: a copy or
a new untitled file at the top waits for no lookup of a path in a folder,
and holds none up. A rename's claim, unlike a create's, still covers its
paths and all under them.

Overlapping creates in one directory could have kept a lookup waiting for
good, as each new claim held it back again. A lookup held back by a claim
is now counted as parked, and a claim made later waits for the parked
lookups under its paths to go ahead before it takes hold. Lookups arriving
when no claim covers them don't park, so they don't hold claims back.

Renames, deletes, and the check that finds a file deleted outside still
take the move lock for writing, so they wait for a create in progress and
none lands during one. The lock is fair, so a lookup that arrives while a
rename waits queues behind it, as before. Two creates in one directory no
longer take turns on the lock; the contents reserve each name with an
exclusive create, so they get different files.

The registry the writes and renames of #830 used is now a small type of
its own, `PathLocks`, with one instance for writes against renames and one
for lookups against creates. The lock order is the move lock, then the
lookups, then the writes, then a path's turn.

This closes #829.
