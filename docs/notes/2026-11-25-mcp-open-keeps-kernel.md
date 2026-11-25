## 2026-11-25, A failed open keeps the kernel of a file it keeps (#947)

Follow-up to #941 (PR #946), which merged before its reviewer's two Lows
were fixed. The #941 entry describes the final design; this change:

- `failed_open` now decides about the file first, under the move lock,
  and shuts down a session the call started only when the file wasn't
  kept because someone has it open. Before, it checked the room outside
  the lock, so a client joining in between kept the file but lost its
  kernel.
- A notebook the call created is kept, not deleted, when the call joined
  a session another client started on it meanwhile ("someone has a
  session on it"). A client that uses contents and sessions but no room
  (a REST-only client) would otherwise keep a session on a file that is
  gone.

Test: `mcp_failed_read_keeps_a_created_notebook_with_a_session_someone_started`
(gated provisioner; fails without the change).
