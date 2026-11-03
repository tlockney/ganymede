## 2026-11-03, Changes since the last read (#313)

`notebook_read` and `notebook_open` end with `@@read <mark>`, and
`notebook_read(since: <mark>)` lists only the cells others added,
edited, moved or deleted since. Prerequisite for the
`ganymede-journal` plugin's `UserPromptSubmit` hook (#315).

- A mark is a snapshot, not the room's Yjs state vector. A state vector
  says which updates a reader has, but not who made the ones it lacks
  in a form we can filter on, and turning updates back into cell
  changes needs history the room doesn't keep (gc is on). A snapshot of
  `(cell id, hash of type and source)` per cell is cheap, and the diff
  is plain: ids missing, ids new, hashes changed, and moves as the ids
  outside the longest common subsequence of the shared ids.
- "By others" works by moving the reader's own mark with its own
  edits: each MCP edit adopts the touched cell's new entry into the
  connection's mark, placed after its nearest predecessor that the mark
  has. That happens in the same room call as the edit, and reads set
  the mark in the same room call as the read, so a person's edit can't
  slip between them and be swallowed.
- One mark per connection and notebook, with a stable name, and any
  connection can read with it. A hook in another process is a different
  MCP connection, so a mark per read would not follow the agent's own
  edits; a stable mark that every read moves does, and keeps the hook
  and the agent from both reporting the same change.
- Marks are in memory (at most 4,096 per server, least recently used
  dropped). An unknown mark returns the whole notebook, which is what
  an agent needs after a restart anyway.
- Outputs aren't compared: a person re-running a cell would otherwise
  show as an edit of every cell after a run-all.
