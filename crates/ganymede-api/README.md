# ganymede-api

The server API as plain functions over `ganymede-kernel-host`, with no HTTP
framework. `ganymede-server` maps these to axum routes; an in-app bridge (the
iPad profile) can call them directly.

- `KernelManager`: kernelspecs, start, get, list, interrupt, restart,
  shutdown, idle culling. A start or restart whose kernel dies or doesn't
  answer is relaunched on fresh ports, up to three tries. A kernel that
  dies on its own is removed and its clients get `status: dead`. Restart
  announces `status: restarting` the way `jupyter_server` does.
- `ClientConnection`: one websocket's worth of traffic, on its own
  unbounded queue that outlives restarts. All clients share the kernel's
  sockets, so shell, control, and stdin replies are filtered by the
  `session` in the client's own headers; iopub goes to everyone.
  `connect_client_as(kernel, session_id)` resumes a dropped connection:
  the queue filled while it was away is replayed, as `jupyter_server`
  buffers it, and a connection still open for that session is
  replaced.
- `SessionManager`: notebook path to kernel, with `jupyter_server`'s
  create-returns-existing rule for a path already bound.
- Models with the exact field names `jupyter_server` returns.
- `TransportFactory` and `StaticKernelspecs`: the seams a test or an app
  uses to run without ZMQ or a filesystem.

Tests run against the fake kernel from `ganymede-kernel-host`.
