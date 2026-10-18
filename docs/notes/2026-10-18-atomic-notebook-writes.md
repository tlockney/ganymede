## 2026-10-18, atomic notebook writes from the CLI (#74)

`ganymede execute --inplace` and `-o` wrote the notebook with
`std::fs::write`, which truncates the file first. A crash, a full disk or
Ctrl-C mid-write left a truncated or empty notebook, and an editor reading
the file at the wrong moment saw half of it. The server's contents layer
already wrote through a temp file and a rename; the CLI did not share it.

- The temp-file, fsync, rename helper moved from `ganymede-contents` into
  `ganymede-nbformat` as `atomic_write`. `Notebook::write_file` (and so the
  CLI's `write_notebook`) and the contents layer both use it.
- A symlink is followed to its target, which is replaced; the link stays.
  The old rename put a regular file in place of the link.
- The temp name `.~<name>-XXXXXX.tmp` is cut to fit in 255 bytes, so a
  file with a long but legal name can still be saved.

Leftover temp files from a killed process are not cleaned up; `tempfile`
removes its file on any error short of that.
