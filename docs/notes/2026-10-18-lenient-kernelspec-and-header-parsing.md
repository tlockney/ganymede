## 2026-10-18, kernelspecs and headers parsed as leniently as jupyter_client (#58)

Ganymede rejected input that `jupyter_client` accepts, and the rejection was
quiet: a bad `kernel.json` was only logged and vanished from
`/api/kernelspecs`, and a kernel reply that failed to decode was dropped, so
the host waited until its timeout.

- `kernel.json`: `display_name` and `language` default to `""`. `env` is a
  loose dict in Jupyter; numbers and booleans are now stringified and other
  values (null, lists, objects) skipped with a warning.
- Kernelspec names are the lowercased directory name, and directories
  whose lowercased name is outside `[a-z0-9._-]+` are skipped, as in
  `_list_kernels_in`. `install` already enforced the same rule; it now
  shares the check.
- `install` copies symlinked subdirectories (it used `DirEntry::file_type`,
  which does not follow links, and then `fs::copy` failed on a directory).
- Message headers: `username`, `date` and `version` default to `""` when
  missing. `msg_id`, `session` and `msg_type` are still required.
- `null` metadata reads as `{}` on the wire, ws v1 and legacy ws paths.

What we learned: for a compatibility target, every required serde field is
a place where we are stricter than the reference. Default what the
reference defaults.
