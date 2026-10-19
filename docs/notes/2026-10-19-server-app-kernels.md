## 2026-10-19, the server serves an app's kernels (#165)

The step after [#160](https://github.com/tlockney/ganymede/issues/160)
on the #59 plan: an app embedding the server (the iPadOS shell) can hand
it the kernels it runs itself.

- `serve::Startup` has optional `kernelspecs`, `provisioner` and
  `transports`. Each unset one falls back to what the server built
  before: kernelspec dirs, `SubprocessProvisioner`, `ZmqTransportFactory`.
- `ganymede-server` has `zmq` and `subprocess` features, default on,
  forwarded to `ganymede-api`. Built without one, `run_with` fails at
  startup unless `Startup` supplies the replacement.
- `tests/embedded.rs` runs the server with an app's kernelspec source,
  provisioner and factory, then lists, starts and restarts its kernel over
  REST; CI runs it with `--no-default-features`.
- `cargo check --target aarch64-apple-ios` of the server fails on Linux
  (rusqlite's bundled SQLite needs `xcrun`), so the iOS build of the
  server stays covered by the macOS `ffi` job.
- `ganymede-ffi` doesn't expose this yet: that comes with the iPadOS
  target and its on-device kernel.
