## 2026-11-07, bound a room client's queue in bytes too (#472)

A room client's outgoing queue held up to `CLIENT_QUEUE_LIMIT` (10,000)
frames. Each client holds its own copy of every frame, so with 1 MB
image outputs a client that stopped reading could hold about 10 GB
before the frame count closed it; the 90 s idle timeout bounds how long,
not how much.

`ClientTx` now also counts the bytes of frames queued and not yet taken
by the socket task, and a send that would take them past the budget
(`max_body_size`, 512 MiB by default) closes the client with 1008, "fell
N bytes behind", as a full frame count does. One frame larger than the
budget still goes to an empty queue, so a big notebook's first sync
isn't refused. The `Hold` client, whose receiver is dropped, never
counts anything: a send to a closed channel gives its bytes back.
