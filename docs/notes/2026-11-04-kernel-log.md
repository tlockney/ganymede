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
the launch is as before. On exit, the provisioner waits up to a second
in all for the pipes to drain, writes the exit line, and only then
reports the exit, so a crash's last stderr lines and its exit line come
before the manager's "restarting" line. Output from something outside
the process group that still holds a pipe is cut off at that point, so
it can't run on into the next process's lines. A line is held to 64 KiB
at most, so a quiet server reading a stream with no newlines doesn't
buffer it whole.

The manager keeps the logs of the last 32 kernels shut down or left
dead: a kernel that crashes at every start leaves the map, and its log
is the reason it died.

The buffer lives in `ganymede-api` (`kernel_log.rs`), one per kernel id,
so a restart's lines follow the old process's. `seq` counts over the
kernel's life, which gives clients a cursor (`after`) to poll with
instead of a stream. With `after`, a page holds the oldest lines after
it, so paging forward skips none still kept; a websocket or SSE variant can come when a client
needs one. Kernels with no pid (in-process, gateway) answer with
`reason: no_process`, the same word the metrics route uses.
