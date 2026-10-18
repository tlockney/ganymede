## 2026-10-18, kernel shutdown steps, cancelled requests, stop signals (#70)

Several small lifecycle gaps from the review, fixed together:

- Shutdown went from `shutdown_request` straight to SIGKILL of the process
  group. It now follows `jupyter_client`'s `finish_shutdown`: the request,
  half the grace period to exit, SIGTERM, the other half, then SIGKILL.
  `KernelProcess` gained `terminate()`, `Unsupported` by default.
- A `request()` future dropped by its caller (a lost `select!` branch, a
  cancelled handler) left its pending entry behind, and `route` kept
  appending that request's iopub output to it until the kernel died. A drop
  guard now removes the entry however the future ends.
- Signals went to `-pid` even after the waiter task had reaped the kernel,
  when the pid (and so the group id) may already belong to someone else.
  The child process now checks it is still alive first, and
  `KernelSession::interrupt` refuses a dead kernel.
- `KernelManager::shutdown_all` shuts kernels down concurrently, so N hung
  kernels take one grace period instead of N.
- `run`, `execute` and `console` handle SIGTERM and SIGHUP: a kernel they
  launched is shut down (an attached one is interrupted), and they exit
  with 128 + the signal. The kernel is in its own process group, so before
  this `timeout 60 ganymede execute ...` or a closed terminal orphaned it.
- A cell that hits `--timeout` is interrupted, so `run --existing
  --timeout N` no longer leaves a shared kernel busy.

Not done: `PR_SET_PDEATHSIG` (Linux sends it when the spawning *thread*
exits, and a tokio worker or blocking-pool thread can retire while the
kernel should live; doing it safely needs a dedicated long-lived spawner
thread), and replay protection (`digest_history`), which is low value on a
signed localhost connection. Moving blocking file I/O off the async
handlers is a separate change.
