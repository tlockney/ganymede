## 2026-11-03, scratch runs don't store history (#294)

`cell_run` with `record: false` went through `ganymede_exec::execute_cell`,
which always sent `store_history: true`. Scratch runs advanced the
kernel's execution counter, so recorded cells showed gaps in their
counts, and a recorded cell could pick up a scratch result through `_`.

`ganymede_exec::Options` now has `store_history` (default true, as
`nbclient`), and scratch runs set it to false. The fake test kernel now
follows ipykernel: only stored runs advance its counter. Before, it
counted every request, so it could not show the bug.
