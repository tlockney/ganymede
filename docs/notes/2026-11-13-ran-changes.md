## 2026-11-13, cells others ran (#777)

Third step of #763. A read mark now also holds a fingerprint of each
code cell's last run (execution count and outputs), so a cell someone
else ran since the agent's read is reported with `"ran"` in its change,
with its new outputs, and counted in the `@@changed` line and the
`notebook_read(since)` summary. The agent's own runs, including
`notebook_run_all` and clearing outputs it marked to leave out, move its
mark after the run finishes; outputs that arrive after a run timed out
show as `"ran"`, which tells the agent they came. Clearing outputs and
count is not a run. Who ran a cell isn't recorded: runs go through the
kernel channels, not the room, so a run alone has no `"by"`.
