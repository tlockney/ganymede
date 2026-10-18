# ganymede-contents

The contents seam of Ganymede ([seams](../../docs/architecture.md#seams)): the `/api/contents`
model as `jupyter_server` defines it, a `Contents` trait, and `FsContents`,
a local-filesystem implementation. No Python, no caching; the files on disk
are the source of truth ([the iPad use case](../../docs/ipados.md)).

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
  - Notebooks are parsed with `ganymede-nbformat` on read and written through
    it on save, so the bytes on disk are what Python `nbformat` writes.
  - Atomic writes: temp file beside the target, fsync, rename; mode of an
    existing file preserved.
  - External-change detection: a `SaveRequest.last_modified` older than the
    file's mtime by more than 500 ms is a 409 `Conflict`. Without it the
    client's copy wins. This is an opt-in extension: `jupyter_server` has
    no such check. The check and the write of one save run under a
    per-path lock.
  - The first save of a notebook creates its checkpoint, as
    `jupyter_server` does, so a bad first edit can be reverted.
  - `new_untitled` and `copy` claim the new name with an exclusive create,
    so a file made in between is not replaced.
  - `checkpoint_dir` moves checkpoints outside the root (iCloud-synced roots
    should not sync them), mirroring the root's directory layout underneath.
  - `notary` turns on notebook trust: code cells are served with
    `metadata.trusted`, notebooks are signed on save when every code cell is
    trusted, and `Contents::trust` signs one unconditionally.
- `trust::Notary`: nbformat's `NotebookNotary`. HMAC-SHA256 over the
  notebook computed exactly as nbformat does (sorted keys, scalars as
  nbformat's `str()`, `metadata.signature` skipped), keyed with
  `notebook_secret` (created mode 0600 if missing), with signatures in
  Jupyter's `nbsignatures.db`. `Notary::open(data_dir)` shares both files
  with Jupyter. The SQLite store needs the `sqlite` feature (SQLite built
  from C by `rusqlite`, which the server enables); without it signatures
  are kept in memory, so the crate still checks for iOS and Android
  without a C cross compiler.

Tests: `cargo test -p ganymede-contents`. They cover notebook, text and base64
round trips (notebook bytes checked against `Notebook::to_file_string()`),
listing shape, untitled and copy naming, rename, delete, traversal and
symlink rejection, hidden paths, conflict detection, checkpoints in both
layouts, and read-only mode. `tests/trust.rs` checks signatures and
`check_cells` against fixtures from Python nbformat
(`tests/fixtures/generate_trust.py`) and the trust rules end to end.

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
- The signature recorded on save is computed over the notebook as it will
  be read back, not over the client's JSON as sent. The two hash the same
  unless the client sends fields nbformat drops on read (`trusted` on a
  markdown cell), where jupyter_server's own signature would not match on
  reopen.
- Not implemented: `hash`/`hash_algorithm` (omitted from the JSON),
  `pre_save`/`post_save` hooks,
  `nbformat` validation messages on read.

## What to try next

- An HTTP layer in `ganymede-server` mapping these calls to the axum routes.
- A file watcher (`notify`) emitting change events for the document seam.
- An iCloud-aware implementation that defers reads of files still
  downloading, if the Files-app profile needs it.
