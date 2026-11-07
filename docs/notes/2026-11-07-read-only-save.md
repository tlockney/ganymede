## 2026-11-07, saving a read-only file is refused (#439)

`atomic_write` renames a temp file over the target, which needs only a
writable directory. A `0444` file in a writable directory was replaced (its
mode put back), while the contents model said `writable: false`.
`jupyter_server` writes in place and returns 403.

`atomic_write` now opens an existing file for writing, without truncating,
before it writes anything, and returns `PermissionDenied` when the OS
refuses. The contents layer turns that into 403 naming the file. Root can
still write a `0444` file, as with `jupyter_server`, so the tests skip
themselves when run as root; CI runs them as a normal user.

The rename still changes the owner and breaks hard links of a file that is
writable; that is a property of atomic replacement, not part of this fix.
