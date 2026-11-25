# Fix round for ganymede#943 (pre-push review findings)

The implementation on this branch is reviewed. Fix every item below. Rules from the original brief
still apply (`git show 7deb36c:HERMES_TASK.md`): tests that fail without each fix (confirm by reverting
the fix once), fmt/clippy/test/doc checks, docs describe what the code does now, update the dev log
`docs/notes/2026-11-25-guarded-runs-recheck.md`. The last commit deletes HERMES_FIX.md.

## M1: `ran_source_hash` must not claim fresh outputs are stale
Only room runs (`Run::cell` in crates/ganymede-server/src/execute.rs) write
`metadata.ganymede.ran_source_hash`. Runs in JupyterLab / the apps (client-side, they write outputs into
the room and blank `execution_count` when a cell starts) leave the old hash behind.
Fix: in the room (crates/ganymede-documents/src/room.rs), when an update that is not the server's own
room-run edit changes a code cell's `execution_count` or outputs, remove that cell's
`metadata.ganymede.ran_source_hash` (keep other `ganymede` keys; remove `ganymede` if it becomes empty
only if it was created by us, otherwise leave the empty object... simplest: remove just the key). Use
the same place that already notices count changes for `clear_stale_clean_run` as the precedent. File
changes that come in via reload/merge keep whatever the file has. Test: room run records the hash; then
a client update (as JupyterLab does: blank count, new outputs) -> key gone, other `ganymede` keys kept.
Also a room run after that records it again.
Docs (rooms.md, dev log, execute.rs module docs): the key is written only by runs the server executes
through the room, and removed when a client runs the cell; explain why it is cell metadata rather than
per-output metadata (stream/error outputs have no metadata).

## M2: untested plain-metadata branch
`schema::set_ran_source_hash` (crates/ganymede-documents/src/schema.rs ~524-533) claims it works when
the cell's `metadata` is a plain value. Add a unit test where a cell holds `metadata` as plain JSON
`{"tags":["t"],"ganymede":{"keep":1}}` (not shared maps) and check tags and `keep` survive with the new
hash. Confirm it fails with the `fill_shallow` line removed.

## L1: `execution_skipped` outliving its cause
Clear `execution_skipped` on the same client-run signal as M1, and when the cell's source is edited.
Test both.

## L2: docs for the API change
- docs/api.md (~452-454, the list of Ganymede's extensions to `/api/kernels/{id}/execute`): add the
  dispatch recheck of guarded ids (a divergence from jupyter-server-documents, which runs the current
  source), the `skipped` key in the `wait` answer, the room-only `execution_skipped`, and the saved
  `metadata.ganymede.ran_source_hash`. Keep the `**Unreleased.**` note rule from CLAUDE.md.
- docs/clients.md (or wherever nb-cli is described): a client that sends hashes without `wait` (nb-cli)
  sees a skipped cell go idle with no outputs and no error; say so.
