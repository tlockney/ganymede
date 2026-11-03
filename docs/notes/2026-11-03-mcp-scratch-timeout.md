## 2026-11-03, Scratch runs interrupt on timeout (#323)

`ganymede mcp`'s scratch runs (`cell_run` with `record: false`) go
through `ganymede_exec::execute_cell`, which stops waiting at the
timeout and leaves the code running: interrupting is the caller's job.
`Kernel::execute_with` in ganymede-engine does interrupt; the MCP
server didn't. A timed-out scratch `while True: pass` left the
notebook's shared kernel busy, so every later run, recorded or not,
queued behind it.

The scratch path now interrupts through the kernel manager and says so
in the reply ("Interrupted the kernel.", or why the interrupt failed).
Recorded runs are unchanged: they keep running after the timeout,
since their outputs still land in the notebook. The fake kernel's
`sleep <ms>` honours interrupts, which makes the test quick: without
the interrupt, the second run would wait out a 30 s sleep.
