## 2026-11-05, case-only rename on case-insensitive volumes (#377)

Renaming `notebook.ipynb` to `Notebook.ipynb` on default APFS or NTFS was
a 409: the destination "exists" because it is the source under a folded
name, and the no-replace rename refuses it. jupyter_server allows it with
`os.path.samefile`.

The rename now treats it as a case change when the two names share a
parent, match ignoring case, are the same entry (device and inode on
Unix, not following a symlink; canonical path elsewhere), and the
directory has no entry spelled exactly like the destination. The last
test tells a folded name from a hard link on a case-sensitive volume,
whose two names are both listed; that stays a conflict. A case change
uses plain `rename`, which APFS and NTFS apply to the name.

The positive test runs only where the temp volume folds case (macOS CI);
Linux CI covers the hard-link conflict.
