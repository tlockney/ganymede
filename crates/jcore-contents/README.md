# jcore-contents

The contents seam of jupyter-core-rs (sketch 7.2): the `/api/contents`
model as `jupyter_server` defines it, a `Contents` trait, and `FsContents`,
a local-filesystem implementation. No Python, no caching; the files on disk
are the source of truth (sketch 6).

## What it does

- `ContentsModel`, `Checkpoint`, `SaveRequest`, `GetOptions`: the wire shapes
  of `GET`/`PUT`/`PATCH`/`DELETE /api/contents/{path}` and its `checkpoints`
  routes, with `jupyter_server`'s field names, nulls and timestamp format
  (RFC 3339 UTC with microseconds). Serialize them straight to JupyterLab,
  VS Code or Juno.
- `Contents`: `get`, `save`, `delete`, `rename`, `new_untitled`, `copy`, and
  the four checkpoint calls, all `async`.
- `Error` with `status()` returning the HTTP status `jupyter_server` would
  send (404, 403, 409, 400, 500), so the HTTP layer maps errors blindly.
- `FsContents::new(root, FsOptions { checkpoint_dir, allow_hidden, read_only })`:
  - Path safety: `..`, absolute paths and backslashes are `Forbidden` before
    any I/O; so is anything a symlink makes resolve outside the canonical
    root, including dangling symlinks.
  - Notebooks are parsed with `jcore-nbformat` on read and written through
    it on save, so the bytes on disk are what Python `nbformat` writes.
  - Atomic writes: temp file beside the target, fsync, rename; mode of an
    existing file preserved.
  - External-change detection: a `SaveRequest.last_modified` older than the
    file's mtime by more than 500 ms is a 409 `Conflict`. Without it the
    client's copy wins.
  - `checkpoint_dir` moves checkpoints outside the root (iCloud-synced roots
    should not sync them), mirroring the root's directory layout underneath.

Tests: `cargo test -p jcore-contents`. They cover notebook, text and base64
round trips (notebook bytes checked against `Notebook::to_file_string()`),
listing shape, untitled and copy naming, rename, delete, traversal and
symlink rejection, hidden paths, conflict detection, checkpoints in both
layouts, and read-only mode.

## Where it departs from jupyter_server, on purpose

- An absolute checkpoint dir in `jupyter_server` puts every file's checkpoint
  in one flat folder, so `a/x.ipynb` and `b/x.ipynb` collide. Here the root's
  directory layout is mirrored under the configured dir.
- `copy` of a notebook copies the bytes; `jupyter_server` round-trips it
  through `nbformat`, which can rewrite an unnormalized file.
- base64 content is emitted without the line breaks `base64.encodebytes`
  inserts; decoding accepts either.
- Non-empty directories are refused with 400, which is `jupyter_server`'s
  behaviour with `delete_to_trash` off; there is no trash here.
- Not implemented: `hash`/`hash_algorithm` (omitted from the JSON), chunked
  uploads (`chunk`), notebook signing/trust, `pre_save`/`post_save` hooks,
  `nbformat` validation messages on read.

## What to try next

- An HTTP layer in `jcore-server` mapping these calls to the axum routes.
- A file watcher (`notify`) emitting change events for the document seam.
- An iCloud-aware implementation that defers reads of files still
  downloading, if the Files-app profile needs it.
