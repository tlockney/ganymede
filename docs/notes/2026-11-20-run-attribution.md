## 2026-11-20, who runs a cell, in the room (#896)

A cell run through the execute endpoint or an MCP tool already showed
`execution_state: running` in the room and went back to `idle` when it
ended. Now it also says who runs it: a code cell's room-only key
`execution_by`, set with the running mark and removed when the cell goes
idle. `strip_runtime` in `ganymede-documents` drops it, as it drops
`execution_state`, so it is never in a saved or returned notebook.

The name comes from the entry point. `RoomCmd::Enqueue` carries `by`. The
MCP tools pass the client's name (`run_all` and `run_cells` in `mcp.rs`).
The execute endpoint uses the tailnet login, else a new body field `by`,
else "an API client" (`runner_name`), each cleaned with `clean_name`, the
same cleaning as a room client's name; one empty after cleaning yields to
the next. A `by` that isn't a string is a 400, from the body's
deserialization.

A cell can be queued more than once (named twice in a batch, or by two
requests), and a run ends for a skipped or deleted cell too. So the room's
`RoomExec` keeps, per cell id, who runs each queued run in order. A cell
with nothing queued gets its name in the same room edit as the running
mark; one already queued keeps the name of the run in progress. Each run
that ends pops the front: the cell stays running, now by whoever is next,
or goes idle and the entry is dropped.

Checked with unit tests in `execute.rs` (one cell, queued twice, given
twice in a batch, skipped, deleted, refused batch, not saved, name
sources and cleaning) and two tests in `tests/http.rs` that follow a
cell through a websocket client of the room: the REST endpoint (body
name, none, empty, tailnet login over the body, non-string 400) and MCP
(`notebook_run_all` and `cell_edit` with `run`). Each piece was reverted
once to see its test fail: the nothing-queued condition, the pop, the pop
for skipped cells, the removal on idle, the entry cleanup, the strip
line, the order and cleaning of the REST name, and each MCP caller's name.
