## 2026-10-18, Ctrl-C stops the server with requests open (#125)

Ctrl-C did not stop `ganymede lab` (v0.2.0). axum's `with_graceful_shutdown`
waits for every open connection to finish its request, with no limit. Idle
keep-alive connections close at once and upgraded websockets are no longer
tracked, so a JupyterLab tab alone did not hang it in headless Chromium; one
connection with a request still in progress did, indefinitely. Reproduced
by sending headers without the closing blank line, then SIGINT: the server
logged "shutting down kernels" and stayed up. A second Ctrl-C did nothing,
because the signal future had already fired and nothing else listened.

`serve_until` now bounds the drain: after the first Ctrl-C or SIGTERM, open
requests get 3 seconds, then the server stops serving and shuts kernels
down; a second signal skips the wait. The signal handlers are installed
once, before the port is bound, and feed a channel, which is also what the
tests drive instead of real signals. Which request a real browser left
open in Thomas's case was not established; the bound covers any of them.
