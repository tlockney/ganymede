## 2026-11-03, parallel_starts keeps the kernels' output (#327)

`parallel_starts` failed again on macOS with `cdp-node #2: kernel
died`, after #261 ruled out the port race. The test built its engine
`quiet`, so the dead kernel's own stdout and stderr were discarded and
the failure said nothing about why it exited.

The kernel host could already mark each line of a kernel's output with
`[kernel <first 8 of its id>]` (`LaunchOptions::prefix_output`, used by
the server), but `Engine` always launched with it off. `EngineBuilder`
now has `prefix_output(bool)`, and `parallel_starts` runs its kernels
unquiet and prefixed. A failure names the kernel's connection file
(`kernel-<id>.json`) and whether the process is still alive, so its
lines can be found in the job's stderr. The kernel's output reaches the
real stderr, not libtest's capture: the host writes to the stderr
handle directly, so it shows in CI logs whether the test passes or
fails. The cause of the exit is still open; this PR only makes the next
failure readable.
