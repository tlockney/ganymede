## 2026-10-19, embedding prep for the Swift shell (#156)

Items 1, 3 and 7 of #59, the first step of the plan for the macOS app
(Rust core in-process, see the
[plan](https://github.com/tlockney/ganymede/issues/59#issuecomment-6256252827)).

- `ganymede-api` has `zmq` and `subprocess` features, on by default,
  forwarding to kernel-host. `ZmqTransportFactory` exists only with
  `zmq`. CI checks api for iOS with neither. The server keeps both on:
  its `run_with` builds the subprocess provisioner and ZMQ transport, and
  until there is another provisioner (#160) a server without them could
  not start a kernel.
- Browser wasm still fails for api, through `ganymede-contents`' getrandom
  (#159); WASI builds.
- `Handler::on_input` is async (`async_trait`, since `execute_with` takes
  `&mut dyn Handler`). The terminal still reads stdin in `block_in_place`.
- `Notary::open` without `sqlite` logs a warning that signatures last only
  for the process.
