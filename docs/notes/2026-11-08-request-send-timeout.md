## 2026-11-08, request timeout covers the send (#535)

`KernelSession::request` started its timeout after `sink.send` returned. A
ZMQ DEALER to a dead kernel blocks a send once its queue fills, so such a
request hung with no timeout. The deadline now starts before the send and
covers both it and the reply; a send that misses it fails with
`Error::SendTimeout`. `a_request_whose_send_blocks_times_out` uses a sink
whose `send` never finishes.

Review follow-ups: the request wraps the send and the reply in one
`tokio::time::timeout`, which handles huge timeouts (a `now() + timeout`
deadline panicked on overflow), and labels the timeout by whether the send
finished. `a_slow_send_comes_out_of_the_reply_wait` checks they share one
deadline. `Kernel::execute_with` no longer interrupts the kernel after a
send timeout: the request never reached it, so the interrupt would hit
someone else's cell.

Second review: the send timeout is its own variant, `Error::SendTimeout`,
rather than a `Timeout("send")` label other crates compared as a string,
so the compiler holds that contract. `ganymede-exec` keeps it out of
`CellTimeout`, so the MCP scratch run no longer says the code will run
later (the request was dropped), and the REST 504 says the kernel did not
accept the request.
