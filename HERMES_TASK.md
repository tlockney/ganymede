# Task: ganymede#943, queued room runs must not run a source other than the one that was checked

Issue: https://github.com/tlockney/ganymede/issues/943 (read it and Thomas's decision comment).
Decision (Thomas): a guarded request whose cell changed while queued is REJECTED at dispatch
(skipped, reported as "Cell changed while queued"). No snapshot execution.

## Code
`crates/ganymede-server/src/execute.rs` (read the module doc first). `enqueue()` checks each
`Selector::Id(id, Some(hash))` against the room's current source (murmur2 seed 0 of the source, as a
decimal string) and returns ids; `RoomExec::handle(Enqueue)` sends a `Batch { cells: Vec<String> }` to
`run_worker`; `Run::cell` asks the room for the cell as it is now (`RoomCmd::Get`) and runs it; outputs
come back by cell id (`Outputs`/`Append`/`Done`). `docs/rooms.md` documents the endpoint.

## Design
1. Guard carried with the queue: `enqueue` returns, per queued id, the accepted hash
   (`Option<String>`: Some only for an id selector that gave `source_hash`; tag/"all"/unguarded ids are
   None). `Batch.cells` becomes `Vec<(String, Option<String>)>` (or a small struct). `run_and_wait`
   (MCP/other internal callers) stay unguarded.
2. Recheck at dispatch: in `Run::cell`, after `Get`, if the entry has a guard and
   `murmur2(cell.source) != guard`, don't run it. Treat it like a failed cell: `done(room, id, None,
   None)` with a "changed" marker, return false so the rest of the batch is skipped (later cells may
   depend on it; same as stop_on_error), and the batch's `finished` reports false.
3. Room event: the skipped cell gets a room-only cell field `execution_skipped` =
   `"Cell changed while queued"`, set in the room when `Done` carries the changed marker (extend
   `RoomCmd::Done` with e.g. `changed: bool`). It is never saved: exclude it exactly where
   `execution_by` is excluded (find every place: `grep -rn execution_by crates`), and it is removed
   when the cell is queued again (in `enqueue`, where `execution_state` is set to running) or when the
   cell's source is edited? (removing on next enqueue is enough; say so in docs).
   Note cells skipped because an earlier cell failed/changed do NOT get this field.
4. `wait` response: when the batch had changed cells, add `"skipped": [{"cell_id": "...",
   "error": "source_changed_while_queued", "message": "Cell changed while queued"}]` to the JSON
   (omit the key when empty). Carry this through the `finished` oneshot (change `bool` to a small
   result struct, keep `RunEnd.all_ok` semantics for `run_and_wait`).
5. Executed-source identity on the run's outputs: when a cell finishes running (not skipped), record
   the murmur2 of the source that ran in the cell's metadata as `metadata.ganymede.ran_source_hash`
   (string, same format as `source_hash`), merging with any existing `metadata.ganymede` object (keep
   its other keys). This IS saved (it's what lets a client tell, after reload, that outputs belong to an
   older source: stale iff `ran_source_hash != murmur2(current source)`). Pass the hash from `Run::cell`
   (it has the `Cell` it ran) through `Done`. A run where the cell was edited mid-execution therefore
   keeps the hash of the source that ran.
   Check that this metadata write does not clear the notebook's `clean_run` stamp (cell metadata
   doesn't count, per docs/rooms.md) and does not break existing tests that compare saved notebooks
   byte-for-byte; update those tests' expectations where a server-run cell is involved.
6. Unguarded requests keep today's behaviour (run the current source at dispatch). Document it.

## Tests (execute.rs unit tests and/or the existing server integration tests for the execute endpoint;
find them: `grep -rn "source_hash" crates/ganymede-server/tests crates/ganymede-server/src`)
1. Hold the first queued cell open (a kernel/stand-in that blocks until released, or queue behind a
   long-running cell with a barrier), edit the next queued cell's source via the room, release: the
   guarded run of the edited cell does not run, gets `execution_skipped`, `wait` response lists it with
   `source_changed_while_queued`, and later cells in the batch don't run.
2. Same but unguarded: the edited source runs (today's behaviour).
3. Edit during execution: outputs land and `metadata.ganymede.ran_source_hash` equals the hash of the
   source that ran, not the edited one.
4. `execution_skipped` is never in the saved file, and is cleared when the cell is queued again.
5. Existing `metadata.ganymede` keys on a cell are kept.

## Docs
`docs/rooms.md`: the execute section: guard rechecked at dispatch, skip + `execution_skipped`, `wait`'s
`skipped`, `ran_source_hash`, unguarded behaviour. `**Unreleased.**` note (CLAUDE.md) for the new
section(s). Dev log `docs/notes/2026-11-25-guarded-runs-recheck.md`.

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
