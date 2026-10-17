# jcore-api

The server API as plain functions over `jcore-kernel-host`, with no HTTP
framework. `jcore-server` maps these to axum routes; an in-app bridge (the
iPad profile) can call them directly.

- `KernelManager`: kernelspecs, start, get, list, interrupt, restart,
  shutdown, idle culling. Each `RunningKernel` has a broadcast hub that
  outlives restarts, so a client's subscription survives the session being
  replaced. Restart announces `status: restarting` on the hub the way
  `jupyter_server` does.
- `ClientConnection`: one websocket's worth of traffic. All clients share
  the kernel's sockets, so shell, control, and stdin replies are filtered by
  the `session` in the client's own headers; iopub goes to everyone.
- `SessionManager`: notebook path to kernel, with `jupyter_server`'s
  create-returns-existing rule for a path already bound.
- Models with the exact field names `jupyter_server` returns.
- `TransportFactory` and `StaticKernelspecs`: the seams a test or an app
  uses to run without ZMQ or a filesystem.

Tests run against the fake kernel from `jcore-kernel-host`.
