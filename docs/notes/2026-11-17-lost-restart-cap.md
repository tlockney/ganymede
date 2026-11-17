## 2026-11-17, Cap kernel restarts after lost (#844)

A kernel the platform ended (`lost`) restarted without a count, so one that
dies at every start (a web view that runs out of memory loading) restarted
without end, and clients saw only the same `restarting`.

- `Core::crashed` counts losses apart from crashes: `lost_restart_limit`
  (3) within `lost_restart_window` (60 s) of the last loss. The fourth in a
  row leaves the kernel dead. Both are `KernelManagerConfig` fields only;
  there is no CLI flag or TOML key, since only an app that registers kernels
  can see a loss.
- Both counters are a `RestartCount`, which also replaces the crash
  counter's tuple. The window is exclusive: a loss exactly one window after
  the last counts from one.
- Signal: `status: restarting` keeps `"reason": "lost"` (released in
  v0.11.0). `status: dead` now carries it too, when the cap is hit or when
  `restart_limit` is 0. Crash restarts and crash-limit deaths have no
  `reason`; clients must treat a missing one as a plain death. No new
  message type or state, so JupyterLab and nbclient see the usual statuses.
- The cell that was running gets "The system ended the kernel, and it is
  being restarted." or, when capped, "...too often; it was not restarted."
  The kernel log says `restarting it (n/limit)` or `leaving it dead`.
- A loss before the first `kernel_info_reply` is still a failed start,
  capped by the three launches in `Core::boot`.
- The test double ends a session with a process that reports `lost`. It
  needs a pid, or the dead kernel's log isn't kept for the log route.
- Follow-up: the notebook-side notice is ganymede-ui#469.
