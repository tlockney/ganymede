## 2026-11-07, JupyterLab workspace files named as jupyterlab_server names them (#492)

`/lab/api/workspaces/<id>` saved to `<id>.jupyterlab-workspace` with every
character but ASCII letters, digits, `-`, `_` and `.` replaced by `_`. So
`a/b` and `a:b` shared `a_b.jupyterlab-workspace` and replaced each
other, and an id longer than a file name allows answered 500.

The file name is now `jupyterlab_server`'s `slugify(id)`: lowercased,
reduced to letters, digits, `_` and `-`, cut to fit 128 bytes with the
extension, then `-` and four hex digits of the id's SHA-256. The
signature keeps `a/b` (`ab-c14c`) and `a:b` (`ab-6783`) apart, and the
names match what JupyterLab writes to the same `lab/workspaces` folder.
Two differences remain, both noted on `workspace_slug`: non-ASCII
characters are dropped instead of NFKC-folded, and `%` escapes are not
decoded a second time.

A workspace saved under the old name still loads when its metadata id
matches, and its old file is removed when it is next saved or deleted,
so the listing doesn't show it twice.
