## 2026-10-18, extension names from wheels are validated (#46)

`extract` took any directory under `labextensions/` in a wheel as an
extension name, so a wheel shipping `labextensions/.packages/...` made the
install delete the record directory and put its own files there. A forged
record could then point a later `remove` at any path, since record
`extensions` entries were joined to the root unchecked.

- An extension name is now `name` or `@scope/name`, each part of
  `[A-Za-z0-9._~-]` and not starting with `.` or `_` (npm's rules, with
  uppercase allowed for old packages). That excludes `.`, `..`, the
  `.packages` records and the `.<key>-*` staging directories. A wheel with
  any other name fails to install rather than being partly unpacked.
- The same check runs on every record read: a record naming anything else
  is ignored as if absent, and `remove_extension` refuses such a name, so
  nothing outside `labextensions/` is ever removed.
- Taking over another package's extension now needs `--force`; before, a
  dependency pulled in automatically could replace
  `@jupyter-widgets/jupyterlab-manager` with only a warning.
- `ZipFile::enclosed_name` (zip 9) drops a leading `/` and `.`
  components rather than rejecting them, so `/x` and `./x` land inside the
  staging directory; only `..` that would escape returns None. Symlink
  entries are written as regular files holding the target text.
