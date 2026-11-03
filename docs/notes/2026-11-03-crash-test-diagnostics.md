## 2026-11-03, Crash test failures say what arrived (#285)

`a_crashed_cdp_kernel_restarts_as_under_jupyter_server` stalled once in
CI for 180 s on "running a cell on the restarted kernel": frames kept
coming, but never both the `execute_reply` and `idle` for that cell. The
panic didn't say which server it was running against or what the frames
were, and 27 local runs (the cdp test alone and the whole `server`
suite) didn't reproduce it.

So the failure now carries the evidence. `crash()` prefixes each step
with the server (`jupyter_server` or `ganymede`). `exchange_step` takes
an overall deadline, and when it or the 60 s per-frame limit runs out it
reports whether the reply and `idle` came, this request's iopub message
types, and a count of the other parents' messages by channel, type and
execution state. The outer `within` bound is 200 s so the 180 s deadline
inside fires first with that report.

The first CI run with the report caught it on macOS, under
jupyter_server: `running a cell on the restarted kernel: no reply and
idle in 180 s (reply: true, idle: false, this request's iopub: [], other
messages: {})`. The shell reply came; not one iopub message did, for
this request or any other, so the new websocket got none of the
restarted kernel's iopub. That is the oracle's restart path. The test
already doesn't wait for `idle` from jupyter_server after the crash (it
can leave the kernel `busy`), so on that side the exchange now ends at
the reply, and Ganymede still has to send `idle`. If Ganymede ever
stalls the same way, the report names it.
