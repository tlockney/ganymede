# Fix round for ganymede#945 (pre-push review findings)

The implementation on this branch is reviewed. Fix every item below. Rules from the original brief
still apply (`git show e2e6c67:HERMES_TASK.md`): tests that fail without each fix (confirm by reverting
the fix once), fmt/clippy/test/doc checks, docs describe what the code does now, update the dev log
`docs/notes/2026-11-25-room-recovery-copies.md`. The last commit deletes HERMES_FIX.md.

## High
H1. Copies are per root. All servers share `<data_dir>/recovery`, and `Meta.path` is relative to each
server's root, so one project's copies show up (and can be read, restored over, pruned) in another.
Fix: store copies under `<recovery dir>/<root key>/`, where root key = hex of a stable hash (sha2 is in
ganymede-server's deps) of the canonicalized root path; list/get/discard/prune/open-time lookup only see
that subdir. Also record the absolute notebook path in `Meta`. Tests: two Collabs on different roots,
one recovery dir: neither lists, reads, discards nor prunes the other's copies; same relative path in
both stays separate. Fix the dev log's justification sentence accordingly.

## Medium
M1. A merge conflict whose copy can't be written must keep `save: "failed"` (with the error) until a
copy is written or the person acts; `reload`'s reset to "saved" and the next successful save must not
clear it. Return a flag from `keep_conflicting`; keep a sticky "lost edits" state on `Disk`. Tests: blocked
recovery dir + conflict -> state stays failed after the merge and after a later good save; plus a test of
the normal reload-to-saved branch.
M2. A room whose content isn't a notebook (every save fails "not a notebook") must still close after
`GIVE_UP_AFTER`, with an error log, as on main (no copy is possible). Test it.
M3. Shutdown: in `Cmd::Flush` (flush_all), if the room is still dirty after its save (failed, or Skipped
because read-only), write a copy (`SaveFailed` / `ReadOnly`) before exiting; log an error if that fails.
Test through `flush_all`.
M4. Add tests for: the `serve.rs` wiring (production server keeps copies: tests/embedded.rs with
`GANYMEDE_DATA_DIR` set to a tempdir, or equivalent), the `read_only` close reason, the `recovered_at`
guard (no second copy when a client arrives during the close), and the cleanup-path `FileGone` reason.
M5. Ids must be strictly increasing within a process: `max(now_micros, last + 1)` with an atomic, then
the random suffix. Fix both bound tests so they don't depend on sub-microsecond timing. Run each 50
times to show they are stable.
M6. docs/rooms.md ~119 must match the code: only a delete through the contents API (`Cmd::Removed`)
closes the room without a copy; a deletion found by the cleanup tick keeps the room open until a copy
is written.
M7. `**Unreleased.**` notes (CLAUDE.md format) beside the new docs/api.md recovery entry (~460) and the
changed merge text / alt text in docs/rooms.md (~52-59).
M8. `saving` is not implemented: remove it from the `room.rs` doc comment (~691) and any docs; the dev log
says why (the app shows "Saving" itself, ui#808).

## Low
L1. A read-only dirty room's save (`Skipped`) sets `save: "failed"` with save_error "another server owns
the file".
L2. Discarding the copy `state.recovery` names: re-list and name the next newest copy of that path, or
remove the key if none is left. Test.
L3. `prune` returns the ids it deleted; call `forget_recovery` for each so no room names a deleted copy.
Test.
L4. `merge_disk`'s "all" conflict (room content isn't a notebook): log an error and set `save: "failed"`
with an explanatory error.
L5. If the recovery dir can't be created at server start (e.g. an unwritable data dir in the iOS app),
log a warning and run without recovery (behaviour as before), rather than keeping rooms open forever.
Test.
L6. docs/reference.md (~680, `GANYMEDE_DATA_DIR`): say it also holds recovery copies.

## Nits
- collab.rs ~4150: Fixture doc mentions a nonexistent `with_dir` parameter; fix.
- collab.rs ~4322: `assert!(!dir.exists())` on a path nothing writes can't fail; assert something real.
