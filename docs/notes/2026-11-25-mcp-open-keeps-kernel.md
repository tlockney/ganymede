## 2026-11-25, A failed open keeps the kernel of a file it keeps (#947)

Follow-up to #941 (PR #946), which merged before its reviewer's two Lows
were fixed. The #941 entry describes the final design; this change:

- `failed_open` now decides about the file first, under the move lock,
  and shuts down a session the call started only when the file wasn't
  kept because someone has it open. Before, it checked the room outside
  the lock, so a client joining in between kept the file but lost its
  kernel.
- A notebook the call created is kept, not deleted, when the call joined
  a session on its path ("someone has a session on it"): one another
  client started meanwhile, or one left from an earlier file at that
  path, since deleting a file doesn't end its session. A client that uses contents and sessions but no room
  (a REST-only client) would otherwise keep a session on a file that is
  gone.

Tests (each fails without the change):

- `mcp_failed_read_keeps_a_created_notebook_with_a_session_someone_started`:
  a gated provisioner; the file is kept for a joined session.
- `mcp_failed_read_keeps_the_session_of_a_notebook_someone_has_open`: the
  call starts the session, another connection holds the room, and the read
  then fails because the contents' `get` does (a test wrapper, switched on
  after the room opened); the file and the session stay.
