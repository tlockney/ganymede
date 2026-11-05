## 2026-11-05, contents API edge cases from a probe (#371)

A script sent odd requests to a `main` build's REST API: encoded
`..`, NUL bytes, missing parents, bad notebooks and base64, chunk
numbers out of order, concurrent writes and renames, unknown kernel,
session, terminal and room ids. Nearly all answered as jupyter_server
does. Three didn't:

- `/api/contents/` with the trailing slash was a 404: axum's `{*path}`
  doesn't match an empty path, so the root needs its own route for each
  spelling.
- Moving a directory into itself reached `rename(2)`, whose `EINVAL`
  became a 500. The check is now explicit, before the call, using
  `path_under` (so `sub` → `subway` still works).
- A name over the filesystem's limit gave a 500. `io::ErrorKind::InvalidFilename`
  (stable since Rust 1.87) now maps to 400, which covers any write path.

Concurrent saves to one file always left one whole write, and twenty
concurrent renames onto one name gave one 200 and nineteen 409s.
