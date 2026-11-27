## 2026-11-25, clean-run stamp fingerprint (#944)

A `clean_run` stamp described the notebook only while a room was watching
it: a file edited while no room was open kept a stamp that no longer held.
The stamp now carries a fingerprint of the cells, and a room checks it
whenever it takes content from the file. Decision (Thomas, on the issue):
the fingerprint covers cell types, sources and order, not outputs.

What changed:

- `schema::source_fingerprint` returns `v1:` and eight hex digits of
  `murmur2` (seed 0) over `{type_len}:{cell_type}{source_len}:{source}\n`
  for each cell in order. Lengths are in bytes so no two layouts give the
  same bytes. A cell the crate can't type (`Cell::Unknown`) counts with its
  raw `source`, a string or an array of strings joined. No new dependency.
- `schema::clean_run_status` and `schema::CleanRun` tell what a stamp says
  about the cells: `None`, `Current`, `Stale` (no fingerprint, one that
  isn't `v<digits>:`, or a `v1:` one that doesn't match) and `Unverified`
  (a real `v<digits>:` prefix of a version other than 1, which is left
  alone, never reported as current; the docs tell readers to treat it as
  not current). Nothing else reads them yet.
- The `notebook_run_all` description now says the stamp carries a
  fingerprint of the cells (`docs/mcp-tools.md` regenerated).
- `clean_run_edit` in `mcp.rs` writes `fingerprint` into the stamp from the
  notebook it already checks.
- Opening never writes the file. `Room::forget_unfit_clean_run` removes a
  stale stamp from the room only (and resets the room's change flag, so a
  client's sync step 2 afterwards doesn't make it dirty): no `last_edit`, the saved generation and
  the baseline agree it is clean, so there is no autosave and
  `state.dirty` stays false. The file loses the stamp with the next real
  save. `run_room` calls it once the file is open, and a new `relock`
  calls it when `take_lock` turns a read-only room writable (the tick and
  `Moved` paths). It does nothing in a read-only room. The recovery
  copies (#960) and the save state `run_room` sets up are untouched.
- `Room::reload` and `Room::merge_disk` keep making the room unsaved when
  they remove a stamp (private `clear_unfit_clean_run`), so the file is
  rewritten without it. The in-room `run_print` invalidation is unchanged:
  it also covers execution counts.
- `docs/rooms.md` describes the field, the exact byte layout (so a reader
  outside a room can compute it), the opening and reload behaviour and the
  badge's wording, with the unreleased note before the field. It says a
  `GET` of the file returns the stamp unchecked until the room next saves,
  and that clearing outputs alone keeps the stamp (a clear that also
  blanks counts removes it in an open room).

Post-merge nits (main moved to 0.18.0 with #959 and #960):

- `docs/rooms.md`: the `ran_source_hash` entry points record the hash "on
  every cell the kernel takes up", as
  `the_hash_is_recorded_only_once_the_kernel_sent_execute_input` has it.
- `recovery.rs`: the module doc and `Store::claimed` say that a copy under
  a hidden directory is not seen unless the copy's saved path reaches the
  file through a symlink inside the root, as `docs/rooms.md` does.
  `a_copy_saved_under_a_hidden_dir_of_the_root_is_not_seen` covers the
  hidden check on the saved path.

Tests:

- `a_room_opened_on_a_file_changed_since_the_stamp_drops_the_stamp`: a
  changed source, order or cell type in the file while no room is open;
  the stamp leaves the room, the room stays clean (`state.dirty` false),
  and a later edit's notebook has no stamp.
- `a_forgotten_stamp_is_not_in_the_baseline`: a read-only room that
  restores its baseline after a refused edit doesn't bring the stamp back.
- `an_unverified_stamp_is_dropped_on_open_only_without_a_fingerprint`: no
  fingerprint, a wrong `v1:`, an empty string and junk (`abc`, `v:`, `vx:`,
  `1:`) go; `v2:` and `v10:` stay.
- `a_matching_stamp_survives_open_and_outputs_are_not_covered`: outputs
  and counts changed in the file keep the stamp.
- `clean_run_status_tells_stale_from_current`.
- `a_stamp_in_a_reloaded_or_merged_file_is_kept_only_when_it_fits_the_file`:
  `reload` and `merge_disk`, a fitting and a wrong fingerprint.
- `a_client_joining_after_the_stamp_was_forgotten_leaves_the_room_saved`
  (the change flag) and
  `forgetting_a_stamp_keeps_a_room_with_unsaved_edits_unsaved` (the
  `clean` guard).
- `a_room_whose_lock_retry_succeeds_forgets_an_unfit_stamp_without_saving`
  and `a_moved_room_that_turns_writable_forgets_an_unfit_stamp_without_saving`
  (collab, `run_room` with another server holding the lock; each fails with
  its `relock` call site back to `take_lock`).
- `a_read_only_room_leaves_a_stamp_that_does_not_fit`: also checks it when
  the room turns writable.
- `the_source_fingerprint_is_stable` (a hard-coded value, checked against a
  separate Python implementation),
  `the_source_fingerprint_has_no_concatenation_collisions` (one cell
  `a\ncodeb` against the cells `a` and `b`, which only the length prefixes
  separate) and `the_source_fingerprint_reads_an_unknown_cells_source`.
- `opening_a_file_with_an_unfit_stamp_does_not_write_it` (collab, with a client syncing): the
  room's stamp is gone, `state.dirty` is false, the file is unchanged well
  past the autosave delay, and a real edit saves it without the stamp.
- `a_room_that_turns_writable_forgets_a_stamp_that_does_not_fit` (collab,
  through `relock`).
- `opening_a_notebook_drops_a_clean_run_stamp_that_does_not_fit_its_cells`
  (server, through `notebook_open`): after an edit saves, the file has no
  stamp; a matching one stays.
- `mcp_run_all_stamps_a_clean_run_and_an_edit_clears_it` now checks the
  stamp's fingerprint. The run-all tests that seed an old stamp give it
  the fingerprint of their cells, so it survives opening for the run to
  remove.
- The existing reload and merge tests give the stamps they expect to keep
  a matching fingerprint.
