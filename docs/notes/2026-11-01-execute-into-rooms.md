## 2026-11-01, server-side execution into rooms (#244)

`jupyter-server-documents`' (JSD) `POST /api/kernels/{kernel_id}/execute`,
read from its `execution_handlers.py` and `rooms/ynotebook_room.py`
(main, 2026-10-07), and nb-cli's client of it (`ydoc_execution.rs`).

- **Contract.** Body `{document_id, cells: [{cell_id, source_hash}],
  client_id, request_id, previous_request_id}`; `source_hash` is
  MurmurHash2, seed 0, of the UTF-8 source, as a decimal string
  (`ganymede_documents::murmur2`, checked against JSD's Python). All
  cells are checked before any is touched, then cleared, marked
  `execution_state: "running"` and queued; `200 null`. 409
  `{"error": "source_mismatch", "cell_id"}`, 408 when
  `previous_request_id` isn't queued within 10 s, 400 otherwise (a
  markdown cell, an unknown cell, no room). nb-cli probes for the
  endpoint with an empty body and takes a 400 as "present".
- **Atomic batches** come free: the room's task checks and queues in one
  step, and nothing else touches the room meanwhile.
- **Completion edge.** nb-cli treats a cell as done only when
  `execution_state` is `idle` and `execution_count` changed in the same
  update; a cell skipped after an error gets `idle` alone. So counts are
  written with `idle` at the end, not on `execute_input` (JSD does the
  same, to stop JupyterLab clearing `[*]` early).
- **Worker.** One task per room runs batches in order through
  `ganymede-exec`'s `NotebookRun` (display updates across cells, clears
  with `wait`, stream coalescing at the end), on a list of the cells it
  ran, so a display update reaches an earlier cell. It forgets them after
  256. Outputs go to the room through `schema::set_outputs`, which keeps
  unchanged outputs and appends to a stream that grew, so a kernel
  printing a line sends a line. While a cell runs, consecutive streams are
  merged as JupyterLab's output area merges them.
- **Stop on error.** A failed cell stops the rest of its batch, as
  `stop_on_error` does for JupyterLab's Run All. JSD runs every queued
  cell regardless; nb-cli sends one cell per request, so it can't tell.
- **No client needed.** The endpoint opens the room if it isn't open, and
  a room with cells queued or running doesn't close. Outputs are saved
  with the notebook a second after the last one.
- **`/api/fileid/index`** (JSD) and `jupyter_server_fileid`'s
  `/api/fileid/id` and `/api/fileid/path` are served over `FileIds`.
  Answering `index` is what makes nb-cli treat the server as JSD
  (`server_writes_outputs`) and use the endpoint.
- Not done: JSD's `/api/outputs` service (outputs kept outside the
  document), its session-bound kernel (here the request names the
  kernel), and input requests (refused, as JSD's `allow_stdin=False`).
