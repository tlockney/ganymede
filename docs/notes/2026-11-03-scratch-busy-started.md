## 2026-11-03, a scratch run counts its busy status as started (#340)

`ganymede mcp`'s scratch run decided whether the kernel had taken up its
code by whether an `execute_input` or an output for the request came.
`execute_streaming` dropped `status` messages before the handler, so the
request's `busy`, the one message the protocol requires for every
request, wasn't seen. A kernel that sends no `execute_input`, running
code that prints nothing, was reported as "hadn't started" on a timeout
and left running.

`ganymede_exec::Handler` and `NotebookHandler` now have an `on_busy`
method (default: nothing), called when the request's `busy` arrives;
the scratch run counts it as started. A kernel sends `busy` for a
request only when it takes it from its queue, so a request still queued
behind another run is still told apart. The fake kernel's `quiet <ms>`
cell is `sleep` without `execute_input`, to test it.
