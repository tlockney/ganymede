## 2026-11-08, a timed-out request keeps its `busy` (#590)

`execute_streaming` learned that the kernel took a request up from the
broadcast, or after a lag from the request's record. On a timeout the
loop broke as soon as the request future returned, and that future had
already dropped the record, so a `busy` broadcast between the loop's poll
of the subscription and its poll of the request was lost: the cell read
as queued and was left running. `KernelSession::request_with_busy` now
reads the record just before it goes, and says whether `busy` came; with
a reply it reads the reply's statuses. `execute_streaming` calls
`on_busy` from that before it returns, which also replaces its separate
check of the reply's statuses for a lag noticed after the request
finished. The race itself can't be reproduced on demand, so the test is
at the session level: a request the kernel took up but never answered
times out with `busy` reported, and one left queued without.
