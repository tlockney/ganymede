## 2026-11-07, rooms ping clients and bound their queues (#436)

A room's websocket had an unbounded outgoing queue per client, no pings,
no write timeout, and a 1 GiB message limit. A half-open connection
(laptop asleep) never sent `Leave`, so its room never retired and kernel
output queued for it without limit.

- Each client's queue is bounded at 10,000 frames (`ClientTx`, the
  kernel bridge's `CLIENT_QUEUE_LIMIT`). A send that finds it full doesn't
  wait: it notifies the socket task, which closes with 1008 and leaves.
  The kernel bridge drops output past its limit instead; a room can't
  skip a Yjs update, so the client goes and resyncs on reconnect.
- The socket task pings every 30 s and drops a client silent for 90 s.
  Every send has a timeout of 30 s plus 1 s per 64 KiB, so a blocked
  write can't stop the task for long, and a large first sync over a slow
  link still gets through (a flat 30 s would never let it join).
- A room's `Close` that finds the queue full is stashed next to the
  notification, so a deleted file still closes with 4404, not 1008.
- Messages and frames are capped at `max_body_size`, the largest notebook
  the contents API takes. Before, tungstenite's 16 MiB default frame size
  was the real cap, since browsers send one frame per message.
- The queue counts frames, not bytes; a byte budget is a follow-up.

`Collab::with_heartbeat` (hidden) shortens the intervals for the test.
