# Task: ganymede#944, clean-run stamp must not describe a notebook that changed since

Issue: https://github.com/tlockney/ganymede/issues/944 (read it and Thomas's decision comment).
Decision (Thomas): the fingerprint covers cell sources and cell order only (with cell type), NOT outputs.

## Code
- Stamp read/write: `crates/ganymede-documents/src/schema.rs` (`clean_run`, `set_clean_run`).
- Stamp creation: `crates/ganymede-server/src/mcp.rs` `clean_run_edit` / `record_clean_run`.
- In-room invalidation: `crates/ganymede-documents/src/room.rs` (`run_print`, `clear_stale_clean_run`,
  `clear_clean_run_for_file`, `reload`, `merge_disk`, and wherever a Room is created from a file).
- Docs: `docs/rooms.md` "Run from a clean kernel" (says today that nothing checks the stamp on open).

## Design
1. Fingerprint: `pub fn source_fingerprint(nb: &Notebook) -> String` in ganymede-documents (schema.rs
   or a small module), returning `"v1:<8 hex digits>"`: `ganymede_documents::murmur2` (seed 0) over a
   canonical byte string built from every cell in order: for each cell
   `"{type_len}:{cell_type}{source_len}:{source}\n"` (lengths in bytes, so no two layouts collide by
   concatenation). Use the source exactly as nbformat holds it (the joined string). No outputs, ids,
   counts or metadata. No new dependency.
2. Stamp gets `"fingerprint": "v1:..."`, computed in `clean_run_edit` from the room's notebook at
   stamp time (the same `nb` it already reads).
3. Validate when a room takes content from the file: on room open (the path that loads a notebook into
   a new Room), in `Room::reload`, and in `Room::merge_disk`. If the room is writable (not read-only)
   and its notebook carries a stamp whose `fingerprint` is missing, has an unknown version prefix, or
   doesn't match `source_fingerprint` of the current content, remove the stamp as a server edit (follow
   `clear_stale_clean_run`: broadcast, mark changed so the room saves the file without it). A
   read-only room leaves it (its owner decides). Factor this into one function used by all three paths.
4. Keep the existing in-room `run_print` invalidation unchanged (it also covers execution counts).
5. Add a helper other readers can use: `pub fn clean_run_status(nb: &Notebook) -> CleanRun` with
   `None | Current | Stale | Unverified` (Unverified = stamp without fingerprint / unknown version),
   used by step 3 and exposed for the CLI/apps later. Don't wire it into other surfaces in this PR.

## Tests (each fails without the change)
1. Stamp a notebook (through the MCP path or by writing the stamp the way `clean_run_edit` does), save,
   close the room; edit a cell source in the FILE (no room open); reopen the room: the stamp is gone
   (and the saved file no longer has it after the save).
2. A stamp with no `fingerprint` (as older versions wrote) is removed on open (unverified, not clean).
3. A stamp with a matching fingerprint survives open; clearing outputs in the file before reopening
   leaves it (outputs aren't covered).
4. Reordering cells in the file → stale. Changing a cell's type → stale.
5. `reload`/`merge_disk` with a file carrying a different stamp whose fingerprint doesn't match the file
   → removed; one that matches → kept as the file has it.
6. Read-only room: stamp untouched.
7. `source_fingerprint` unit tests: stable value for a fixed notebook (hard-code the expected string so a
   later change to the algorithm is caught), and the concatenation case (`["ab","c"]` vs `["a","bc"]`).

## Docs
`docs/rooms.md` "Run from a clean kernel": add `fingerprint` to the JSON example and describe what it
covers (cell types, sources, order; not outputs, so clearing outputs keeps the stamp), and replace the
"Nothing checks it ... on open" paragraph with the new behaviour. Say what the stamp means:
"Ran from a fresh kernel". `**Unreleased.**` note for the new behaviour. Update the MCP tool description
string in mcp.rs (`notebook_run_all`) only if it would otherwise be wrong. Dev log
`docs/notes/2026-11-25-clean-run-fingerprint.md`.

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
