## 2026-11-01, Kernels started together got the same port (#260)

`parallel_starts` failed on macOS with `KernelDied` on the first
`execute`, after all four kernels had started. The cause was the way ports
are picked. `allocate_ports` binds five random ports to check that they're
free, then releases them for the kernel. Once released, a port can be
picked again by another start running at the same time in this process,
before the first kernel has bound it. Two kernels then get the same port,
and the one that binds second fails.

ipykernel dies on the spot, before `kernel_info`, so the engine's start
retry (#225) covered it. cdp-kernel retries the bind for 5 s instead
(`zmtp.js`, `LISTEN_RETRY_MS`). It binds shell, control and iopub,
answers `kernel_info`, and the start succeeds. When its heartbeat or
stdin port never frees, it exits, and the next request gets `KernelDied`.
Holding the port with a listener reproduces this: cdp-kernel exits after
about 5.2 s.

`allocate_ports` now reserves each port it hands out for 5 minutes in a
process-wide map and won't hand it out again in that time. This is the
job `LocalPortCache` does in jupyter_client. The new
`concurrent_allocations_never_share_a_port` test makes 400 concurrent
allocations. Without the reservation it finds a repeated port every
time; with it, it never does. Other processes can still take a port
between the check and the kernel's bind, and only `ipc` transport closes
that gap. `parallel_starts` now names the kernel in its panic.
