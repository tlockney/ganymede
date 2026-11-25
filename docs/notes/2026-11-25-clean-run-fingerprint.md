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
  same bytes. No new dependency.
- `schema::clean_run_status` and `schema::CleanRun` (`None`, `Current`,
  `Stale`, `Unverified`) tell what a stamp says about the cells. Nothing
  else reads them yet.
- `clean_run_edit` in `mcp.rs` writes `fingerprint` into the stamp from the
  notebook it already checks.
- `Room::clear_unfit_clean_run` removes a stale or unverified stamp as a
  server edit (broadcast, room unsaved), unless the room is read-only. It
  runs when `run_room` has opened the file, in `Room::reload` and in
  `Room::merge_disk`. The in-room `run_print` invalidation is unchanged:
  it also covers execution counts.
- `docs/rooms.md` describes the field and the check, with the unreleased
  note. The `notebook_run_all` tool description stays right and is
  unchanged.

Tests:

- `a_room_opened_on_a_file_changed_since_the_stamp_drops_the_stamp`: a
  changed source, order or cell type in the file while no room is open.
- `an_unverified_stamp_is_dropped_on_open`: no fingerprint, unknown
  version, empty.
- `a_matching_stamp_survives_open_and_outputs_are_not_covered`: outputs
  and counts changed in the file keep the stamp.
- `clean_run_status_tells_stale_from_current`.
- `a_stamp_in_a_reloaded_or_merged_file_is_kept_only_when_it_fits_the_file`:
  `reload` and `merge_disk`, a fitting and a wrong fingerprint.
- `a_read_only_room_leaves_a_stamp_that_does_not_fit`.
- `the_source_fingerprint_is_stable_and_has_no_concatenation_collisions`:
  a hard-coded value (checked against a separate Python implementation)
  and the `["ab","c"]` against `["a","bc"]` case.
- `opening_a_notebook_drops_a_clean_run_stamp_that_does_not_fit_its_cells`
  (server, through `notebook_open`): the file is rewritten without it.
- `mcp_run_all_stamps_a_clean_run_and_an_edit_clears_it` now checks the
  stamp's fingerprint.
- The existing reload and merge tests give the stamps they expect to keep
  a matching fingerprint.
