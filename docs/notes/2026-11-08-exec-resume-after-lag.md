## 2026-11-08, exec: live output resumes after a lag (#521)

Since #507, `execute_streaming` stopped handing iopub messages over live
after a broadcast lag and let the reply's record supply the rest. That kept
order, but one early burst held back all later output until the cell
finished, and an input request after a lag was answered before the prompt
printed ahead of it was shown.

The session records each request's iopub messages before broadcasting them,
so the record is always at least as far along as any subscriber.
`KernelSession::outputs_so_far(msg_id, from)` exposes it while the request
is pending, from index `from` on, so each catch-up clones only the new
tail. On a lag, and before answering an input request, the handler
catches up from it (deduplicated by `msg_id`, as before), and live messages
then follow in order. When the record is already gone (the request finished
between the lag and the catch-up), live handling stops and the reply's
record supplies the rest, as before, with a warning in the log. A lag
before the request is first polled skipped none of its output, so it
changes nothing.
