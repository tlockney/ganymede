## 2026-11-08, no runs in a read-only room (#548)

A room whose notebook another server owns (#272) is read-only and never
saves, yet `execute.rs` `enqueue` still queued cells into it: the execute
endpoint answered 200, MCP `notebook_run_all` reported the run, and the
kernel ran the code, but the outputs went into a room that never saves.
`enqueue` now refuses a read-only room with a message saying where the
notebook is open, which the endpoint returns as a 400 and MCP as an error,
like MCP's edits already do. `docs/rooms.md` says so.
