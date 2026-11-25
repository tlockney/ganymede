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
- Only runs the server executes through the room write that key, and
  JupyterLab and the apps run cells themselves, so an old hash would call
  their new outputs stale. The room (`room.rs`) now removes it when a
  client's update changes a code cell's `execution_count` or outputs:
  the observer of `cells` collects the ids (`rerun_cell`), and
  `Room::clear_run_notes` removes the key and `execution_skipped` in the
  same step. Reading the observed change, not comparing outputs before and
  after, keeps the cost of a keystroke independent of the outputs' size.
  The server's own edits and changes from the file don't remove it. Only
  the key goes; an emptied `ganymede` stays (`schema::clear_ran_source_hash`).
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
  `a_cell_queued_again_shows_no_note_while_its_next_run_waits`,
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
  client, the server and the file (which leaves the room clean).
- `the_ran_source_hash_is_set_and_removed_in_plain_and_shared_metadata`.
