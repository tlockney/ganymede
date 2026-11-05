## 2026-11-05, rename into itself by another spelling (#374)

#372 answered a move of a directory into itself with 400 by comparing API
paths. That misses other spellings of the same directory: `Sub` to
`sub/x` on a case-insensitive volume (default APFS), or to `link/x` where
`link` is a symlink to `Sub` or to a directory inside it. `rename(2)`
refuses those with EINVAL, which mapped to 500.

The rename now canonicalizes the deepest existing ancestor of the
destination's parent, which resolves symlinks, and compares each
directory from there up to the root with the source: by device and inode
on Unix, so case folding needs no special case, and by canonical path on
Windows, whose canonical paths carry the on-disk case. A first version
compared the unresolved ancestors' inodes and missed a symlink into a
subdirectory of the source; the review caught it. Mapping every EINVAL
from `rename` to 400 was simpler but would hide real faults.

The tests use symlinks, so Linux CI covers them; the case-folded form runs
only where the temp volume is case-insensitive (macOS CI). Windows is
unverified by hand.
