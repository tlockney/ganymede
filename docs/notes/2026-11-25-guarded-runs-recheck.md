## 2026-11-25, guarded runs are rechecked at dispatch (#943)

A request that named a cell with a `source_hash` was checked when it
arrived, but the batch can wait behind other runs, and the worker then
ran whatever the cell held. A cell edited in between ran a source nobody
had checked. Thomas decided: such a cell is rejected at dispatch, not run
from a snapshot.

What changed, in `execute.rs` unless said:

- `enqueue` returns each queued id with the hash it was checked against
  (`None` for a tag, `"all"` or an id with no hash), and the batch
  carries it to the worker.
- `Run::cell` compares the hash of the cell it got from the room with the
  guard. On a difference it doesn't run the cell: `Done` carries
  `changed`, the batch stops as after a failure, and the cell is reported.
- The room sets `execution_skipped` = "Cell changed while queued" on that
  cell when its last queued run ends. It is room-only: `strip_runtime`
  in `ganymede-documents` removes it with `execution_by`, and `enqueue`
  removes it when the cell is queued again. The room removes it when a
  client runs the cell, and when the cell's source or type is edited by a
  client, the server or the file. Cells skipped behind it
  don't get it.
- The `finished` channel carries a `BatchEnd` (`all_ok`, `changed`)
  instead of a bool. `wait` adds `skipped` (`source_changed_while_queued`)
  when `changed` isn't empty; `run_and_wait` still reports `all_ok`.
- A cell that ran gets `metadata.ganymede.ran_source_hash`, the hash of
  the source `Run::cell` ran (`schema::set_ran_source_hash`), saved with
  the notebook. A reader can tell outputs are stale when it differs from
  the hash of the current source. Other cell metadata and other
  `ganymede` keys are kept; cell metadata isn't in the room's run
  fingerprint, so the `clean_run` stamp stays.
- Room runs, a notebooks run and the host API write that key for every
  cell they run (`ganymede-exec`'s `set_ran_source_hash`, over a
  `source_hash` moved to `ganymede-nbformat` and re-exported), and a
  run's saved copy removes it from cells the run never started. The
  executor records it only behind its new `record_source_hash` option,
  which those entry points turn on; off, the default, it removes the
  key from the cells it runs instead — keeping the rest of the
  `ganymede` object and dropping an object the removal emptied, and
  leaving one that was already empty — so `ganymede execute` matches
  `jupyter execute` except that it removes a stale `ran_source_hash`.
  The skip note is room-only, so the executor never reads or writes it. Tools
  outside Ganymede (nbconvert, papermill, VS Code, a plain `PUT`) keep
  whatever key they find, so the key says which source ran only when
  Ganymede did the last run.
  JupyterLab and the apps run cells themselves, so an old hash would
  call their new outputs stale. The room (`room.rs`) now removes it when a
  client's update changes a code cell's `execution_count` or outputs:
  the observer of `cells` collects the ids (`rerun_cell`), and
  `Room::clear_run_notes` removes the key and `execution_skipped` in the
  same step. Reading the observed change, not comparing outputs before and
  after, keeps the cost of a keystroke independent of the outputs' size.
  The server's own edits and changes from the file don't remove it. Only
  the key goes; an emptied `ganymede` stays (`schema::clear_ran_source_hash`).
- `enqueue` clears a cell's outputs when it is queued, so it clears
  `ran_source_hash` there too (`schema::clear_ran_source_hash`), as does
  the MCP tools' drop of outputs: a cell skipped after a failure or a
  change has no outputs and no hash. A run that ends writes the hash only
  when the kernel sent `execute_input` (the count is known) and the cell
  is still a code cell.
- `RoomExec` keeps a "changed" flag on each queued run of a cell (with
  the batch it is in), not per cell id. A change marks the cell's next run
  in the same batch, which the stopped batch skips; the note is set only
  when the cell's last queued run ended with the flag. A cell queued by
  two requests, skipped for a change in the first and for a failure in the
  second, has no note. A guarded cell that is deleted or no longer code at
  its turn ends the batch as changed (`not_code_end`).
- It is cell metadata, not per-output metadata, because stream and error
  outputs have no metadata.
- `docs/rooms.md` and `docs/api.md`: a new section each, with the
  Unreleased note. `docs/clients.md`: what nb-cli sees of a skipped cell.

Unguarded requests and the MCP tools are unchanged: they run the source
the cell has at its turn.

Tests (`execute.rs` unit tests, `ganymede-documents/tests/rooms.rs`, and `tests/http.rs` with the kernel held
on a `sleep` cell that is interrupted to release the batch):

- `a_guard_travels_with_an_id_and_only_an_id`: ids with a hash come back
  guarded, tags and `"all"` don't.
- `a_cell_that_changed_while_queued_shows_why_it_did_not_run`,
  `a_cell_queued_again_shows_no_note_while_its_next_run_waits` (the worker's real sequence for `[a#H, a#H]`),
  `the_skip_note_is_not_saved_or_listed`: the room-only note, set only for
  a change, cleared on the next queue, never in the notebook.
- `a_cell_that_ran_records_the_hash_of_its_source`,
  `the_hash_keeps_the_other_keys_of_the_cells_ganymede_metadata` (plain
  and shared-map `ganymede`), `recording_the_hash_keeps_the_clean_run_stamp`.
- `execute_guarded_cell_changed_while_queued_is_not_run`: the edited cell
  doesn't run, the cell after it doesn't run, `wait` lists it, the room
  shows the note on it only, and queuing it again clears the note.
- `execute_unguarded_cell_changed_while_queued_runs_the_new_source`.
- `execute_records_the_hash_of_the_source_that_ran`: a cell edited while
  it runs records the hash of the source that ran, keeps its other
  `ganymede` keys, and the hash is saved to the file.
- `a_client_run_removes_the_hash_and_a_room_run_records_it_again`: a room
  run, a client's update as JupyterLab makes it, and another room run.
- `a_client_run_removes_the_ran_source_hash_and_the_skip_note`: by the
  count, a stream that grows, outputs emptied and replaced; other keys and
  cells are kept, and every client is sent the removal.
- `a_client_change_that_is_no_run_keeps_the_ran_source_hash`: the count
  the cell has, a cell deleted with its outputs, a cell that comes back.
- `the_servers_own_edit_of_outputs_keeps_the_ran_source_hash`,
  `a_file_change_keeps_the_ran_source_hash_the_file_has`.
- `an_edited_source_removes_the_skip_note_and_keeps_the_hash`: by a
  client, the server and the file (which leaves the room clean), and a
  file change into a dirty room (a `merge_disk`, which leaves it dirty).
- `a_note_removed_by_the_room_is_not_content_to_save`: the room's own
  removal of a skip note doesn't count as a change to save.
- In `ganymede-exec`, `a_cell_that_ran_records_the_hash_of_the_source_it_ran`
  covers the executor's side with `record_source_hash` on, over
  `source_hash` moved to `ganymede-nbformat` (with its `murmur2` test)
  and re-exported from `ganymede-documents`;
  `a_run_that_records_no_hash_removes_the_notes_a_cell_had` covers the
  default-off path. In `tests/http.rs`,
  `notebooks_run_saves_a_copy_under_runs` checks a run's copy: the hash
  on a cell that ran, and none on one the run never started.
- `a_cell_cleared_without_running_loses_its_hash`,
  `a_cell_that_stopped_being_code_gets_no_hash`,
  `a_later_run_that_ran_leaves_no_skip_note`,
  `a_cell_that_is_gone_or_not_code_ends_a_guarded_run_as_changed`,
  `a_cell_skipped_for_a_change_then_for_a_failure_shows_no_change_note`,
  `the_ran_hash_is_recorded_only_when_the_kernel_took_the_source`.
- `mcp_clearing_outputs_clears_the_ran_source_hash` (the MCP drop clears
  the key in the room and the saved file) and
  `execute_guarded_cell_deleted_while_queued_stops_the_batch`.
- In `ganymede-exec`, the hash is set when `execute_input` arrives, as the
  room does, and a stale one is removed before the run:
  `the_hash_is_recorded_only_once_the_kernel_sent_execute_input`.
- In `ganymede-exec`, a `ganymede` value that isn't an object is replaced,
  and an already empty one stays.
- `the_ran_source_hash_is_set_and_removed_in_plain_and_shared_metadata`.
