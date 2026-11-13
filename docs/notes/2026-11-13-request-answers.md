## 2026-11-13, open requests and answering them (#783)

Part of #763. `cell_add` takes `answers: <request id>`: the new cell
goes below the request and the request gets
`metadata.ganymede.request = {"done", "answer"}`, which `is_request`
checks, so an answered request is no longer reported. The metadata edit
goes through `own_edit` with no ids, so it moves no mark and credits
nobody: room attribution hashes type and source only. A new tool,
`notebook_requests`, lists the open requests (a person's, unanswered) in
the notebook rooms open under the connection's folder, via a new
`Collab::notebook_rooms`; `ganymede mcp watch` will poll it.

From the post-merge review of #778: `execute::run_and_wait` takes a
future to run when cells still queued at a timeout finish, and MCP
passes its `own_runs`, so the agent's own late runs aren't reported
back to it as `ran`.
