## 2026-11-05, rename into itself by another spelling (#374)

#372 answered a move of a directory into itself with 400 by comparing API
paths. That misses other spellings of the same directory: `Sub` to
`sub/x` on a case-insensitive volume (default APFS), or to `link/x` where
`link` is a symlink to `Sub`. `rename(2)` refuses those with EINVAL, which
mapped to 500.

The rename now also walks the destination's existing ancestors below the
root and compares each with the source by device and inode. That covers
case folding, symlinks and any other alias the filesystem has, without
knowing whether the volume is case-insensitive. Mapping every EINVAL from
`rename` to 400 was simpler but would hide real faults. Windows has no
stable file id in `std`, so there the API-path check is all there is.

The test uses a symlink, so Linux CI covers it; the case-folded form runs
only where the temp volume is case-insensitive (macOS CI).
