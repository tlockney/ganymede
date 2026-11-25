# Fix brief for #943 (pre-push review round 2)

Delete this file in your last commit. Don't push, don't use git stash,
don't touch anything outside this worktree. Commit with clear messages.
rustfmt/clippy may be missing here; format carefully by hand.

## M1: test the `merge_disk` call site
`crates/ganymede-documents/src/room.rs` ~830 calls
`self.clear_run_notes(&before, &HashSet::new())` in `merge_disk`; no test
fails when it is deleted. Extend
`an_edited_source_removes_the_skip_note_and_keeps_the_hash` (or add a
sibling test): make the room dirty first (an unsaved edit to cell `b`),
then `room.merge_disk(&base, &disk, "h")` with cell `a`'s source changed
on disk. Assert `a` loses `execution_skipped` and keeps its hash
(`(kept().0, false)` as the existing test does) and `b`'s note stays.
Check the test fails with the line removed, then restore it.

## M2: Ganymede's own executor must keep `ran_source_hash` right
`crates/ganymede-exec/src/lib.rs` (~line 480, the cell run) keeps a
cell's metadata and only sets `metadata.execution`, so an old
`metadata.ganymede.ran_source_hash` survives a run of new source
(`ganymede execute --inplace`, `POST /api/notebooks/run` save copy,
host API `notebooks.run`).
- Move `murmur2` from `ganymede-documents` into `ganymede-nbformat`
  (keep a `pub use` re-export in ganymede-documents so callers don't
  change), and add one helper there, e.g.
  `pub fn source_hash(source: &str) -> String` (murmur2 seed 0, decimal).
  Make `crates/ganymede-server/src/execute.rs` `source_hash` use it.
- In ganymede-exec, for every code cell it executes, set
  `metadata.ganymede.ran_source_hash` to the hash of the source it sent
  (keep other `ganymede` keys; create the object if missing) and remove
  `metadata.ganymede.execution_skipped`. For cells whose outputs it
  clears without running, remove `ran_source_hash`. Drop an empty
  `ganymede` object if that's what the code does elsewhere (match
  room.rs conventions).
- Test in ganymede-exec: a cell with a stale `ran_source_hash` runs
  edited source; afterwards the key equals the hash of the new source.
  (Use whatever kernel the crate's existing tests use.)
- docs/rooms.md (~234): add that Ganymede's executor (`ganymede
  execute`, notebooks run, host API) records the key too, and that tools
  outside Ganymede (nbconvert, papermill, VS Code, a plain PUT) keep
  whatever key they find, so the key is only reliable when Ganymede did
  the last run. Same adjustment to the claim in
  docs/notes/2026-11-25-guarded-runs-recheck.md (~29).

## L1: test the `content_changed` reset
`room.rs` ~948 `content_changed.store(false)` at the end of
`clear_run_notes` has no test. Add one: reload with a source change that
removes a skip note, then send a state-only client update (or a no-op
`room.edit`) and assert `!room.is_dirty()` and `!fx.changed`. Check it
fails with the line removed.

## L2: anchor without an apostrophe
docs/api.md ~475 heading "The execute endpoint's second check" slugs
differently on VitePress. Rename it to
"### Execute checks the hash again before each run" (no apostrophe) and
update the link at ~455 to `#execute-checks-the-hash-again-before-each-run`.

## Nit 1
docs/api.md ~453-455: move the sentence about the second check out of
the released "What `ganymede serve` answers" bullet into the new
section (which has the Unreleased note), leaving only a link if needed.

## Checks
`cargo test -p ganymede-nbformat -p ganymede-exec -p ganymede-documents
-p ganymede-server` must pass.
