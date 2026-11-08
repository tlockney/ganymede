## 2026-11-08, Shell waits for control and the heartbeat (#501)

`parallel_starts` still failed on macOS with a cdp-node kernel that couldn't
bind one of its ports: cdp-kernel retries a taken port for five seconds,
answers `kernel_info` meanwhile, then exits. The start had already
succeeded, so the kernel died under the test's first execute. The port's
holder showed as "unknown" (no socket `lsof` could see), so the port
reservations (#327) can't help there.

The ZMQ transport's shell socket already waited for stdin and iopub before
sending anything. It now also waits for control to connect and for the
heartbeat port to accept a TCP connection (the same probe the sockets use
before connecting). A kernel still waiting on one of those ports doesn't
receive the start's `kernel_info_request`, so its death fails the start,
and `Relaunch::start` (and the server's start) retries on fresh ports.
Every Jupyter kernel binds all five ports before it can serve, so the wait
costs a started kernel nothing.
