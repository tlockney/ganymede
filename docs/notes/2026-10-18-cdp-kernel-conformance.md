## 2026-10-18, more cdp-kernel scenarios, a nudge that backs off (#69)

The startup nudge re-sent `kernel_info_request` every 500 ms until shell
and control both answered. cdp-kernel answers on shell only once its
runtime is up, on one serial loop, so a slow Node start queued a request
per round for up to a minute and then answered each with a busy/idle
pair. Each unanswered request now waits twice as long as the last
(0.5 s up to 4 s), and shell and control are asked at the same time
rather than one after the other. A fake kernel whose shell is silent for
30 s (paused clock) received 51 requests before and about ten now.

`kernels.rs` gained the cdp-kernel `e2e.py` scenarios that a Rust host
can drive, on cdp-node and python3 alike: queued cells aborted after an
error (and not with `stop_on_error: false`), input without stdin, an
interrupt while blocked in a prompt, the runtime exiting mid-cell (cdp
relaunches it; for ipykernel the kernel dies and the session says so),
`interrupt_request` and `shutdown_request` on shell, a silent execute
publishing nothing, comms to an unknown target closed, a throwing
handler, a frontend `comm_close`, binary buffers echoed through a comm,
history tail and search, display then update in order with prints
around them, and four kernels started at once.

The shell `shutdown_request` scenario failed on macOS with `KernelDied`:
the kernel replies and exits, and the session saw the exit and failed
every waiting request before the reply had come through the sockets.
Now, when the process exits, requests still waiting get up to 500 ms for
their replies before the session is marked dead. A `jupyter_client`
client reads the reply off the socket whether the process has gone or
not. A unit test has the process exit 50 ms before its reply.

Learned: ipykernel answers `interrupt_request` on shell by sending
SIGINT to its own process group, which interrupts that handler, so no
reply comes; that check runs on cdp-kernel only.

Not done: debugger requests, malformed stdin frames, kernel death and
parallel starts through `serve`, and a CI run against cdp-kernel `main`.
