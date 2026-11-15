## 2026-11-15, grant stamp locks scoped to their paths (#829)

A rename through the server used to take one server-wide write lock, so
it waited for every save and copy in flight on any path. The lock prefers
writers, so while a rename waited for one slow save, every new save of
every other file queued behind it. A rename now waits only for the writes
registered under its old and new paths, a directory's included, and holds
back only new writes under those two paths. Saves and copies register the
paths they write: a save its file, a copy its source and its destination
(or the destination directory, when the new file's name isn't known yet),
and a new untitled file its directory. Prefixes match whole path
segments, so a rename of `d` doesn't wait for `dd/x.ipynb`. A rename
whose wait is dropped gives its paths back.

The same stamps remember at most 256 paths whose saves are over, and the
cleanup that keeps that bound ran on every save that ended once more than
256 paths were known, saves in flight included. With more than 256 saves
at once it evicted nothing, yet moved its floor to the latest sequence
number every time, so every look at a grant came back unstable and stale
grants were kept until the load dropped. The cleanup now counts quiet
paths, those with no save in flight, and runs only when more than 256 are
quiet. It evicts only those, and its floor is the latest end among the
paths it evicted, so any number of saves in flight leaves the floor alone.

Still open on #829: item 2, which concerns the move lock, and item 4,
which concerns forgetting a new file's grants and walking the other
roots. This change touches neither.
