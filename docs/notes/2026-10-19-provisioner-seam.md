## 2026-10-19, kernels without ZeroMQ: the provisioner seam (#160)

Item 2 of #59, so an in-process kernel (the iPadOS app's) can be started
and restarted by the engine like any other.

- `TransportFactory` moved from `ganymede-api` to kernel-host (api
  re-exports it), with `ZmqTransportFactory` in `kernel_host::zmq`.
  Provisioner and factory are a pair: the provisioner brings a kernel up,
  the factory connects to it.
- `LaunchedKernel.connection` is `Option<ConnectionInfo>`; `None` for a
  kernel with no sockets. `kernel_host::in_process()` is that launch
  (no process, interrupts over control). The api test double used to
  invent TCP ports for it.
- `EngineBuilder::transports` sets the factory (ZeroMQ by default).
  `Engine::start`, `start_with`, `execute_notebook` and
  `Kernel::restart` no longer need the `zmq` feature; `attach` and
  `connect` still do, since they take connection info.
- `tests/in_process.rs` starts, executes, restarts and shuts down such a
  kernel; CI runs it with `--no-default-features`.
- The server still builds its own subprocess provisioner and ZMQ
  factory. Letting an embedder pass its own is the next step, with the
  iPadOS target.
