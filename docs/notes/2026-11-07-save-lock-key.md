## 2026-11-07, the save lock is the file's (#510)

Saves of one file run one at a time under a lock kept per path. The key
was the path as named, so two names for one file, a symlink inside the
root or a directory symlink, or another case on a case-insensitive
volume, took different locks and could interleave their writes. The key
is now the canonical path: the file's when it exists, else its canonical
directory joined with the name. Found in the 0.7.0 code review (#440).
