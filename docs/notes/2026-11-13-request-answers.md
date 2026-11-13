## 2026-11-13, open requests and answering them (#783)

Part of #763. `cell_add` takes `answers: <request id>`: in one room
edit, the new cell goes below the request and the request gets
`metadata.ganymede.request = {"done", "answer", "id"}`. `is_request`
counts it done only when `id` is the cell's own, since paste, duplicate
and split keep metadata under a new id. The edit is refused unless the
id is an open request from a person, and through
`own_edit_checking`, which checks cells for unseen changes without
crediting or adopting them, when the person changed the request since
the agent's read. Room attribution hashes type and source only, so the
metadata edit credits nobody. A new tool, `notebook_requests`, lists
the open requests (a person's, unanswered) in the notebook rooms open
under the connection's folder, via a new `Collab::notebook_rooms`;
`ganymede mcp watch` will poll it. With `path`, a notebook no room has
open lists nothing rather than opening one: who wrote a cell is known
only to a room.

From the post-merge review of #778: `execute::run_and_wait` takes a
future to run when cells still queued at a timeout finish, and MCP
passes its `own_runs`, so the agent's own late runs aren't reported
back to it as `ran`. The future holds only the read mark's token and
the marks, not the connection, so it doesn't keep the connection's
rooms open.
It adopts once the whole batch is done; adopting each cell as it
finishes would also keep a person's re-run of an early cell, made while
later ones are queued, from being taken as the agent's (follow-up).
