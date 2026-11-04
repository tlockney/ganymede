## 2026-11-04, kernel log (#365)

`GET /api/kernels/{id}/log` is a Ganymede extension for the Mac app's
kernel log tab (tlockney/ganymede-ui#248): Jupyter Server has no API for
a kernel's own stdout and stderr, which until now reached only the
server's stderr.

The kernel host stays free of buffers and clocks: `LaunchOptions.output`
is a callback (`KernelOutput`) the subprocess provisioner calls with each
line and its source, plus `host` lines when the process starts and when
it exits (code or signal). With a callback, both pipes are always read,
and still echoed to the server's stderr unless `quiet`; without one,
the launch is as before. The exit line waits up to a second for the
pipes to drain, so a crash's last stderr lines come before it; a
grandchild holding the pipe open can't hold it up for longer.

The buffer lives in `ganymede-api` (`kernel_log.rs`), one per kernel id,
so a restart's lines follow the old process's. `seq` counts over the
kernel's life, which gives clients a cursor (`after`) to poll with
instead of a stream; a websocket or SSE variant can come when a client
needs one. Kernels with no pid (in-process, gateway) answer with
`reason: no_process`, the same word the metrics route uses.
