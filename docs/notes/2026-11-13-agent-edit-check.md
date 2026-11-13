## 2026-11-13, refuse unseen edits and flag others' changes (#764)

First step of tracking #763, from the design note
[An agent that responds to your edits](../live-agent-edits.md).

`own_edit`, which every MCP edit goes through, now checks the cells it
is about to change against the connection's read mark before it edits:
a cell whose source or type differs from the mark, or that the mark
lacks, was changed or added by someone else since the agent read it, so
the edit is refused with the cell as it is now. New cells (`cell_add`,
`cell_run`) aren't in the room yet, so they are never refused, and
`cell_clear_outputs` names no cells for the marks, so it isn't either.
The check and the edit happen in the same room call, so no edit lands
between them.

Every tool reply but the reads ends with an `@@changed` line per
notebook the connection holds a mark for that others changed. That
needed `call_tool` written out instead of the one `#[tool_handler]`
generates, to wrap the router's reply. It computes a diff per marked
notebook per call; a connection holds one or two.

The line is a pointer rather than the changes, so a run of edits by a
person doesn't fill every reply, and reading stays the one thing that
moves the mark.
