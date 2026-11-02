## 2026-11-01, A stalled crash test hung e2e for 43 minutes (#265)

`a_crashed_ipykernel_restarts_as_under_jupyter_server` hung on one run
until the job was cancelled; a re-run passed. `crash()` bounded only
the frames it read after the crash cell. Its HTTP calls (a
`reqwest::Client` with no timeout), websocket connects and the first
`exchange` could wait forever, and so could the test server's `Drop`,
which sent SIGTERM and then called `wait()` with no limit. The orphans
left at cleanup (the test binary, `jupyter-server` and `python`) fit a
jupyter_server that didn't exit on SIGTERM while its kernel was
restarting.

Now each step in `crash()` runs under `within(secs, step, ..)`, which
fails the test with the name of the step that stalled. The HTTP client
has no timeout of its own, which would fire first and lose the step
name. `exchange` gives up after 60 s without a frame; `crash()` calls it
as `exchange_step`, which puts the step name in that message too, under
a 180 s bound on the whole exchange (on the jupyter_server path it covers
the restart). `Drop for Server` kills the server with SIGKILL if it
hasn't exited 10 s after SIGTERM. Kernels run in their own process
groups (Ganymede's `process_group(0)`, jupyter_client's
`start_new_session`), so a killed server's kernel can still be left
behind; putting test servers in their own group wouldn't reach them,
and would keep Ctrl-C from reaching the servers.

The e2e jobs have a 15-minute
limit: over the last 40 green runs they took at most 5.

The flake itself isn't reproduced. The next time it happens, the panic
message says which step stalled.
