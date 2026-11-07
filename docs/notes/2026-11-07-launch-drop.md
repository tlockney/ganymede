## 2026-11-07, a cancelled launch left its kernel running (#505)

Dropping a `LaunchedKernel` removed the connection file and nothing else,
so a start cancelled after the fork (a client giving up mid-request, a
timeout around `Relaunch::launch`) left a kernel running that nothing
could reach. The drop now kills a process that is still alive through
`KernelProcess::kill_now`, as `tokio`'s `kill_on_drop` would, and the
engine's own `Kernel` drop is now a second line. Every existing path
that drops a launch shuts the kernel down first, so they see no change.
Found in the 0.7.0 code review (#440).
