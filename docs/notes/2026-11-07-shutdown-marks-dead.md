## 2026-11-07, a failed SIGTERM or SIGKILL still ends the session (#487)

`KernelSession::shutdown` returned as soon as `terminate` or `kill`
failed with anything but `Unsupported`, before `mark_dead()` and before
closing the transport. The session then read as alive though the server
had given up on it, and a failed SIGTERM never escalated to SIGKILL.

Now a failure is logged and kept: a failed SIGTERM goes on to SIGKILL,
the session is marked dead and its transport closed on every path, and
the last error is returned at the end.
