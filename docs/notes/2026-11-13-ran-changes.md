## 2026-11-13, cells others ran (#777)

Third step of #763. A read mark now also holds each code cell's
execution count, so a cell someone
else ran since the agent's read is reported with `"ran"` in its change,
with its new outputs, and counted in the `@@changed` line and the
`notebook_read(since)` summary. The agent's own runs, including
`notebook_run_all` and clearing outputs it marked to leave out, move its
mark after the run finishes. Only counts are compared, so a display
update from another cell isn't a run, and no outputs are serialized on
each tool call. Clearing outputs and
count is not a run. Who ran a cell isn't recorded: runs go through the
kernel channels, not the room, so a run alone has no `"by"`.
