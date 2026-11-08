## 2026-11-08, Shell waits for the heartbeat port (#501)

`parallel_starts` still failed on macOS with a cdp-node kernel that couldn't
bind one of its ports: cdp-kernel retries a taken port for five seconds,
answers `kernel_info` meanwhile, then exits. The start had already
succeeded, so the kernel died under the test's first execute. The port's
holder showed as "unknown" (no socket `lsof` could see), so the port
reservations (#327) can't help there.

The ZMQ transport's shell socket already waited for stdin and iopub before
sending anything. It now also waits, for up to 10 s, until the heartbeat
port answers a ZMTP handshake: a bare TCP accept would pass for a port
another process listens on. A kernel still waiting on that port doesn't
receive the start's `kernel_info_request`, so its death fails the start,
and `Relaunch::start` (and the server's start) retries on fresh ports. A
clash on shell or control was already caught, since the start's nudge needs
a reply on both, and stdin and iopub were already gated.

After the 10 s grace, longer than cdp-kernel's retry, shell sends anyway,
so a kernel whose heartbeat port never answers here (not served, or an
attached kernel with only some ports tunnelled) still starts, as it does
with jupyter_client. The general fix is in cdp-kernel, which could answer
on shell and control only once all its channels are bound.
