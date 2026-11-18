## 2026-11-18, reference jupyter_server picks its own port (#869)

`e2e (ubuntu-latest)` failed once with "server at http://127.0.0.1:41051/
did not come up in 75 s" for the reference jupyter_server. The test
chose its port with `free_port()`, which binds port 0, reads the port
and drops the listener, then started jupyter_server on it. Between
the drop and jupyter_server's bind, seconds of Python startup later,
any other process can take the port; the other tests run in parallel
and their kernels bind five ZMQ ports each. With `port_retries=0`
jupyter_server then exits 1, and its output went to `Stdio::null()`,
so the test polled a dead port for 75 s and panicked with nothing to
go on. Holding the port open in the old code reproduces exactly that
message after 75 s.

The test now passes `--ServerApp.port=0` and reads the real port from
the `jpserver-*.json` jupyter_server writes to the test's own runtime
dir. That narrows the window but does not close it: jupyter_server 2.x
binds the port, closes it, writes the info file, and only then binds
again to listen. And whatever took the port in between may be listening
on it, so a TCP connection proves nothing. The test therefore puts a
file named after its unique root tempdir in the server's root and takes
the port only once an authenticated `GET /api/contents/<that file>`
answers 200, which no kernel socket or other test's server can do; the
file is removed before the test runs. Meanwhile it watches the process:
if jupyter_server exits with "already in use" in its log, whether the
thief listens or only binds, the start is tried again, three times in
all. Any other exit panics with the exit status and jupyter_server's
log, which now goes to a file instead of `Stdio::null()`; no answer
within `CALL_SECS` kills the server and panics with the log. Any
`jpserver-*.json` in the runtime dir counts, not just the one named
for the child's pid, so a `GANYMEDE_E2E_JUPYTER` wrapper that doesn't
exec is found at start-up (stopping still signals only the wrapper,
as before). `free_port()` had no other callers and is gone. Caveat: the
failed CI run's output was discarded, so its cause can't be confirmed;
this race is the cause consistent with what was seen.
