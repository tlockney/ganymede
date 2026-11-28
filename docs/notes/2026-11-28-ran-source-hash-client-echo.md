## 2026-11-28, ran_source_hash and clients that echo outputs (#958)

Since #943 the room removed `metadata.ganymede.ran_source_hash` from a
code cell whenever a client's update touched the cell's `outputs` (any
change: an append, a stream's text growing, a replacement) or its
`execution_count`. A client that writes outputs back after a server run,
as JupyterLab or the apps do when they normalise or merge stream
outputs, made server runs lose the key as soon as that client looked at
them — though the client ran nothing. The risk is quiet: a notebook
whose outputs look stale in one client, and fine in every other.

The rule is narrowed. A client update counts as the client running or
clearing the cell, and removes `ran_source_hash` and `execution_skipped`,
only when it:

- sets `execution_count` to another value (unchanged: the same count is
  no run, null is a run, as JupyterLab does at the start of a run and on
  Clear Outputs), or
- leaves the cell's outputs empty: cleared, replaced by an empty list, or
  missing. JupyterLab's own run and Clear Outputs empty the outputs, so
  they are still caught.

Any other outputs change — an output appended, a stream's text grown, a
non-empty list in their place — keeps the key.

`room.rs`: `rerun_cell` looks the cell up first, then asks. The count
part is as before; the outputs part is new: a change under `outputs`
counts only when the cell's `outputs` in the transaction is now empty
(`outputs_empty`: missing, a `YArray` of length 0, or an empty
`Any::Array`). Reading the observed change and the end state, rather
than comparing outputs before and after, keeps a keystroke's cost
independent of the outputs' size. `clear_run_notes` is unchanged: what
counts as a run is `rerun_cell`'s call.

Tests (`crates/ganymede-documents/tests/rooms.rs`):

- `a_client_run_removes_the_ran_source_hash_and_the_skip_note`: the
  count cleared, outputs emptied (with the count written back as it is),
  outputs replaced by an empty array, and outputs emptied then one
  appended with the count cleared in one transaction. The append case
  moved out: it keeps the key now.
- `a_client_that_echoes_outputs_keeps_the_ran_source_hash`: an appended
  stream, outputs replaced with the list the cell has, replaced with a
  merged stream, and the count it has with an append in the same
  transaction.
- `a_client_change_that_is_no_run_keeps_the_ran_source_hash`,
  `the_servers_own_edit_of_outputs_keeps_the_ran_source_hash`,
  `a_file_change_keeps_the_ran_source_hash_the_file_has`: unchanged.
- `tests/http.rs`: `mcp_clearing_outputs_clears_the_ran_source_hash` still
  passes; a cell whose outputs are emptied still loses the key.

Validation: `validation/jupyterlab.mjs` (CI's `workstation` job) now,
after its own save, has the server run the cell while JupyterLab has the
notebook open (`PUT /api/collaboration/session/<path>` for the file id,
`GET /api/sessions` for the kernel, `POST /api/kernels/<id>/execute` with
`document_id` and `wait`), waits, saves from the page, and fails unless
the saved cell still has `metadata.ganymede.ran_source_hash`. On this Mac
(cdp-kernel, JupyterLab 4.6.6, jupyter-docprovider 3.0.4, headless
Chromium) it printed:

```json
{"opened":true,"kernelIdle":true,"output":"hi from 42","saved":true,"serverRun":true,"hashKept":true}
```

The same step passes against a build of the old rule: this JupyterLab
takes the server run's outputs into the shared document without writing
them back, so the step guards the save that follows a server run but does
not by itself reproduce a client echo. The unit tests are what pin the
narrowed rule; the step is kept because a client that does echo must not
lose the key on that path either.
