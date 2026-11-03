## 2026-11-03, scratch runs don't store history (#294)

`cell_run` with `record: false` went through `ganymede_exec::execute_cell`,
which always sent `store_history: true`. Scratch runs advanced the
kernel's execution counter, so recorded cells showed gaps in their
counts.

`ganymede_exec::Options` now has `store_history` (default true, as
`nbclient`), and scratch runs set it to false. The fake test kernel now
follows ipykernel: only stored runs advance its counter. Before, it
counted every request, so it could not show the bug.

`store_history: false` is not a full sandbox: IPython's displayhook
still sets `_` (and `Out` for the current count) from an expression
result, and every kernel keeps a scratch run's variables. docs/mcp.md
and the `cell_run` tool description say so.
