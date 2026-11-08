## 2026-11-08, the API pump no longer drops replies (#568)

The API's pump read the session's broadcast, which skips messages for a
receiver more than 1024 behind. A skipped `execute_reply` or `idle`
status never reached the client, so its cell stayed at `[*]`.
`KernelSession::subscribe_lossless` gives a receiver an unbounded channel
of its own, fed by `route` before the broadcast, and the pump uses it.
The pump does no I/O (it hands each message to the clients' own
byte-bounded queues, #472), so the channel only absorbs bursts. Other
consumers (exec, the engine) keep the broadcast: they recover lagged
messages from the request's record.
