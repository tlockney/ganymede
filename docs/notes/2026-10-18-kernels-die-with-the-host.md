## 2026-10-18, kernels die with the host, a second Ctrl-C stops (#70)

The rest of #70, after the kernel shutdown steps entry:

- A host killed with SIGKILL, or one that panicked past its cleanup,
  orphaned every kernel; only kernels that watch `JPY_PARENT_PID` noticed.
  On Linux the kernel now asks for SIGKILL when its parent goes
  (`PR_SET_PDEATHSIG`, set between fork and exec). Linux sends that when
  the *thread* that forked the child exits, so kernels are no longer
  forked from whatever tokio thread called `launch`: one
  `ganymede-kernel-spawner` thread, started on first use and never
  stopped, does every spawn, inside the caller's runtime so tokio still
  watches the child. A test re-runs the test binary as a host, launches a
  kernel from it, SIGKILLs it, and checks the kernel goes; another checks
  a kernel outlives the thread that launched it. Elsewhere it is a plain
  spawn. The signal reaches the kernel only, not its process group.
- Whatever a kernel left in its process group (a background job) is
  SIGKILLed when the kernel exits, as `jupyter_client` kills the group.
  Done right after the reap: a group id is not reused while the group has
  members.
- `run` and `execute`: a second Ctrl-C within 2 s stops the kernel (a
  launched one is shut down, an attached one interrupted) and exits 130,
  so a cell that ignores interrupts no longer needs `kill`. The fake
  kernel's `hang <ms>` is a `sleep` that ignores interrupts.
- Replay protection: like `jupyter_client`'s `digest_history`, a signed
  message whose signature was already seen on the connection is dropped
  (`wire::decode_once`, `DigestHistory`, 65536 signatures, a tenth culled
  past that).
