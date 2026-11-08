## 2026-11-08, one rule for "the kernel took it up" (#588)

The engine's `Tracked` and MCP's `Started` each decided whether a
timed-out request had started, from its `busy` or any output, and both
learned it from `execute_streaming`'s broadcast subscription. A
subscriber that lagged past the `busy` never saw it, so a running cell
could be reported as queued: MCP would skip the interrupt, and the engine
would return `Queued`. The session's pending record keeps a request's
statuses without lag, so `KernelSession::busy_seen` reads it, and after a
lag `execute_streaming` calls `on_busy` from the record when the
broadcast skipped it. It calls `on_busy` once per request. Both trackers
now go by `on_busy` alone, the rule its doc states. Output always
follows `busy`, so dropping the output check loses nothing.
