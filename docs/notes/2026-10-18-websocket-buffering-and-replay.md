## 2026-10-18, kernel output kept across a websocket drop (#53)

A websocket client subscribed to a per-kernel `broadcast` from the moment
it connected, and `session_id` in the URL was ignored. When a laptop slept
or a tailnet path changed, the output and `execute_reply` produced while
it was away never arrived, and cells stayed `[*]`. A slow client (a
backgrounded tablet) lagged the broadcast and lost messages, its own
replies included, with only a server-side warning.

What `jupyter_server` does (from its source, `MappingKernelManager` and
`ZMQChannelsWebsocketConnection`):

- The connection's `session_key` comes from the `session_id` query
  parameter. When the last websocket of a kernel closes, `on_close` calls
  `start_buffering(kernel_id, session_key, channels)`: that websocket's
  own ZMQ streams keep running and append every message (iopub, shell,
  stdin) to a plain list. Nothing bounds or expires it.
- The next connection calls `get_buffer(kernel_id, session_key)`. If the
  key matches, it takes over the streams, nudges, and replays the list
  ("Replaying N buffered messages"). If not, `stop_buffering` discards
  it ("Discarding N buffered messages"). Shutting the kernel down discards
  it too.
- A new connection whose `session_key` is already open closes the old one
  ("Replacing stale connection").
- There is no lag: each websocket has its own sockets, and tornado queues
  writes without limit. Output is dropped only by `iopub_msg_rate_limit`
  and `iopub_data_rate_limit`, and the client is told with a `stream`
  message on stderr.

What changed:

- The kernel's broadcast hub is gone. `forward` puts each message on every
  client's own unbounded queue (`bridge::Clients`): iopub for all of
  them, replies only for the session in the parent header, as before.
- `connect_client_as(kernel, session_id)` follows the three points above.
  When the last client of a kernel drops, its queue stays registered and
  keeps filling. The same session id takes the queue over, so there is no
  gap and nothing is delivered twice. Any other connection, or the kernel
  ending, discards it. The websocket handler passes `?session_id=`, and
  closes a replaced socket with 1000 "replaced by a new connection".
- Replies are never dropped. Past 10 000 queued messages, iopub is
  dropped, and when the client catches up it gets a stderr `stream`
  saying how many messages it missed, with the parent of the next one, so
  it shows in the running cell. That is our version of the rate-limit
  notice.
- The engine's streaming handler tracked replayed output by count, so
  after a lag it showed some messages twice and missed others. It now
  tracks msg_ids, and the reply's record fills in exactly the missed ones.

Left: the pump from the kernel-host session's own broadcast can still
lag, but it only moves messages onto queues. A buffer has no time limit,
as in `jupyter_server`; the culler still shuts the kernel down.
