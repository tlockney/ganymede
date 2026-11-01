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
fails the test with the name of the step that stalled, the HTTP client
has a 30 s timeout, and `Drop for Server` kills the server with SIGKILL
if it hasn't exited 10 s after SIGTERM. The e2e jobs have a 15-minute
limit: over the last 40 green runs they took at most 5.

The flake itself isn't reproduced. The next time it happens, the panic
message says which step stalled.
