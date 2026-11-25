# Fix round for ganymede#944 (pre-push review findings)

The implementation on this branch is reviewed. Fix every item below. Rules from the original brief
still apply (they are in the commit that added HERMES_TASK.md: `git show 312effe:HERMES_TASK.md`):
tests that fail without each fix, fmt/clippy/test/doc checks, docs describe what the code does now,
update the dev log `docs/notes/2026-11-25-clean-run-fingerprint.md`. The last commit deletes HERMES_FIX.md.

## Design decisions (made by the thread; implement as written)

1. Opening must not write the file (M1, L2). On room open (`run_room` in
   crates/ganymede-server/src/collab.rs), an unfit stamp is removed from the ROOM but the room stays
   "saved": don't set `last_edit`, and make the room's saved generation / baseline / `state.dirty`
   agree that it is clean (so no autosave and clients see `dirty: false`). The file loses the stamp only
   with the next real save. So a read-only MCP agent or a viewer never causes a write. reload/merge_disk
   keep today's behaviour (they already make the room unsaved). Add tests: open with an unfit stamp ->
   stamp gone from the room, file unchanged on disk after the autosave delay, `state.dirty` false; then an
   edit + save -> file has no stamp.
2. Unknown fingerprint versions are left alone (L4): a stamp whose `fingerprint` has a prefix other
   than `v1:` is NOT removed (never reported as Current; `clean_run_status` returns Unverified). Only a
   missing fingerprint or a `v1:` that doesn't match is removed. Update tests.
3. Read-only -> writable (L1): when `take_lock` turns a read-only room writable (tick and `Moved`
   paths), run the same open-time check (room-only removal as in 1). Test it.
4. Unknown cell types (L3): `source_fingerprint` reads the source of a `Cell::Unknown` from its raw
   value (string, or array of strings joined), not "". Test it.

## Docs (M2, M3, L5, L6) in docs/rooms.md
- Qualify lines ~287-292 ("A file that carries a different stamp ... leaves the stamp as the file has
  it"; "A file that holds one opens with it ...") so they agree with the fingerprint check, pointing to
  the fingerprint paragraph.
- Line ~232: call the badge "Ran from a fresh kernel" (not "reproduced from a clean kernel").
- Document the exact fingerprint layout so a reader outside a room can compute it: per cell in order
  `"{type_len}:{cell_type}{source_len}:{source}\n"`, lengths in UTF-8 bytes, source joined from its lines,
  MurmurHash2 seed 0, `v1:` + 8 lowercase hex digits; and say readers outside a room (e.g. REST GET of a
  file with no room open) should check it themselves.
- Say opening never writes the file; the stamp leaves the file with the next save.
- Put the `**Unreleased.**` note before the new `fingerprint` field description (it must cover it).

## Test nit (N1)
`crates/ganymede-documents/tests/rooms.rs` ~2575: the concatenation case must be one that only the
length prefixes separate, e.g. one cell with source "a\ncodeb" vs two cells "a" and "b" (check the exact
layout makes these collide without lengths), so the assertion fails if the prefixes are removed.
