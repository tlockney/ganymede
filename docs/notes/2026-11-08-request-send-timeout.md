## 2026-11-08, request timeout covers the send (#535)

`KernelSession::request` started its timeout after `sink.send` returned. A
ZMQ DEALER to a dead kernel blocks a send once its queue fills, so such a
request hung with no timeout. The deadline now starts before the send and
covers both it and the reply; a send that misses it fails with
`Timeout("send")`. `a_request_whose_send_blocks_times_out` uses a sink
whose `send` never finishes.
