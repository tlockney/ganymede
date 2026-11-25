# Task: ganymede#945, rooms must not drop unsaved edits (save failures, same-cell merge conflicts)

Issue: https://github.com/tlockney/ganymede/issues/945 (read it). App half is ganymede-ui#792: not here.

## Code
`crates/ganymede-server/src/collab.rs`: module doc (top), `run_room` (the empty-room branch near
`GIVE_UP_AFTER` that closes "with unsaved edits: its file is gone / its saves keep failing", and the
read-only branch "closing a read-only room with edits it couldn't save"), `after_save`, `save_room`,
`reload` (logs "the room's edits to {} lost to it" and returns `lost`). `Room::merge_disk` in
`crates/ganymede-documents/src/room.rs` returns the conflicting parts; the room's own version of them is
in `ours` before the merge. Room-only state lives in the Y doc's `state` map (`dirty`, `path`, `hash`;
see schema.rs doc and `Room::set_state`). Find how `Collab` gets its runtime/room-lock dir
(`with_room_locks`) and how the server binary configures it, and do the same for a recovery dir.

## Design
1. Recovery dir: `Collab::with_recovery_dir(dir)`, set by the server to `<Ganymede data dir>/recovery`
   (persistent across reboots; NOT beside the notebook; find the existing data-dir helper, e.g. the one
   `ganymede info` prints as data_dir). No recovery dir configured (tests, embedded) = behaviour as today
   plus the log line, except where a test sets one.
2. Recovery copy: `recovery::write(dir, path, notebook, reason, detail) -> Result<RecoveryId>`: writes
   `<id>.ipynb` (the room's notebook as nbformat JSON, the same bytes a save would write) and
   `<id>.json` (`{"path", "at" (UTC RFC3339), "reason": "save_failed"|"file_gone"|"read_only"|
   "merge_conflict", "detail" (last error / conflicting cell ids), "notebook": "<id>.ipynb"}`). `id` is
   time-based and unique. Bounded: at most 20 copies per notebook path and 200 MB total; when over,
   delete the oldest (log it). Write via tmp+rename. A failed recovery write is logged at error level and
   the room is then KEPT open (never silently lose).
3. Save failures: in the empty-room branch, before closing a room whose edits aren't saved (file gone,
   saves keep failing, read-only with edits), write a recovery copy. If the write fails, don't close:
   keep the room and retry on the next cleanup tick. So no path retires a dirty room without a
   recovery copy on disk.
4. Merge conflicts: in `reload`, when `merge_disk` will produce conflicts, write a recovery copy of the
   room's notebook as it was BEFORE the merge (reason `merge_conflict`, detail = conflicting parts).
   Take the snapshot only when the room is dirty (conflicts need unsaved edits). Then emit the room
   event (step 5).
5. Save state for clients, in the room's `state` map (room-only, never saved; check the save path
   excludes state as today): `save` = `"saved" | "saving" | "failed"`, `save_error` = last error string
   (removed when a save succeeds), and `recovery` = `{"id", "at", "reason", "cells": [ids]}` for the most
   recent recovery copy of this notebook (merge conflict, or found on open: when a room opens on a path
   that has recovery copies, put the newest one's summary there). Set `saving` only if cheap; `saved`
   and `failed` are required. Changes go out to clients like other state changes.
6. Restore/discard over HTTP (new network surface; justify it in the dev log): follow the style and auth
   of the existing `/api/collaboration/rooms` routes (same auth layer):
   - `GET  /api/collaboration/recovery?path=<api path>` lists copies for a path (metadata only),
   - `GET  /api/collaboration/recovery/<id>` returns `{meta..., "content": <notebook JSON>}`,
   - `DELETE /api/collaboration/recovery/<id>` discards it, and clears `state.recovery` in an open room if it
     named that id.
   Validate `id` strictly (only the characters your id generator makes; no path separators, no `..`):
   ids are file names in the recovery dir. Restore = the client takes `content` and writes it with the
   normal contents/room APIs; no restore endpoint.
7. Update the collab.rs module doc ("the disk wins that cell" now also keeps the room's version in a
   recovery copy; "A room with no clients is saved and dropped" now never drops unsaved edits without a
   recovery copy).

## Tests (each fails without the change; use tempdirs for recovery and data dirs)
1. Saves fail (read-only file / failing Contents stand-in), all clients leave, cleanup delay passes past
   GIVE_UP_AFTER: a recovery copy exists with the room's edits (the cell source you typed), reason
   `save_failed`, and its `.ipynb` parses as a notebook.
2. File deleted on disk with unsaved edits, clients leave: recovery copy with reason `file_gone`.
3. Recovery write fails (recovery dir is a file / read-only): the room stays open (doesn't retire).
4. Same-cell conflict: room edits cell A (unsaved), disk changes cell A: recovery copy holds the room's
   cell A source; `state.recovery` is set in the room with reason `merge_conflict` and cells [A].
5. `state.save` becomes `failed` with `save_error` after a failed save, `saved` and no `save_error`
   after a good one; `state` keys never appear in the saved .ipynb.
6. Bound: writing 21 copies for one path leaves 20 (oldest gone).
7. HTTP: list/get/delete work; delete clears `state.recovery` in an open room; ids with `/`, `..`,
   `%2F`, or unknown chars are 400/404; unauthenticated requests refused like /api/contents.
8. Opening a room on a path with a recovery copy sets `state.recovery`.

## Docs
`docs/rooms.md`: a "Saving and recovery" section (save state keys, recovery copies: where, when, bound,
the three endpoints). Also docs/reference.md / the REST reference page if endpoints are listed there
(`grep -rn "/api/collaboration/" docs`). `**Unreleased.**` note per new section. Dev log
`docs/notes/2026-11-25-room-recovery-copies.md`.

## Rules for this task (all tasks)

- Read the repo's CLAUDE.md and CONTRIBUTING.md first (ganymede: also docs/development.md).
- Work only on this branch. Do not push. Do not open a PR. Commit when done (one or more commits);
  the last commit must delete HERMES_TASK.md.
- No new dependencies. No `unsafe`. Keep public API minimal (`pub(crate)` where possible).
- Match the codebase's style: short doc comments that say what the code does now, in plain sentences;
  existing helpers over new ones (search before writing).
- Every behaviour below gets a test that FAILS without the change. Check that by stashing the non-test
  change and running the test once. Concurrency tests use barriers/channels or paused time, never sleeps.
- No empty catch / `let _ =` / `.ok()` on a write the person relies on: report the failure.
- Before finishing, run what CI runs:
  - ganymede: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
    `cargo test -p <crates you touched>`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
    If disk gets low, `cargo clean` is fine.
  - cdp-kernel: `npm run check`, `npm test` (or the touched test files if the full run is too slow; say so).
- Docs: every sentence describes what the code does now. ganymede dev log: new file
  `docs/notes/2026-11-25-<topic>.md` starting `## 2026-11-25, <topic> (#<issue>)`, listing what changed and
  each test. Never edit docs/notes.md. A feature not in the latest release gets the `**Unreleased.**`
  note described in CLAUDE.md, once per new docs section.
- When done, print a report: what changed (files), which test proves which behaviour, which tests you
  confirmed fail without the change, anything in this brief you didn't do and why.
