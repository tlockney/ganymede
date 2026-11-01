## 2026-11-01, ganymede-ffi without processes on iOS (#208)

- `ganymede-ffi` depended on `ganymede-server` with its default
  features, so the iOS slices of the XCFramework compiled in ZeroMQ and
  subprocess launching, which an iOS app can't use. The 2026-10-19 note
  said #160 removed them; it added the seam but left the FFI crate's
  features alone.
- The FFI crate now has `zmq`, `subprocess` and `gateway` features, on
  by default and passed through to the server. `build-xcframework.sh`
  builds the iOS targets with `--no-default-features --features
  gateway`; `zeromq` is no longer in that build's dependency tree.
- Turning the features off alone would have broken the iPad app: it
  starts the embedded server with no `gateway_url` when its kernels run
  in the web page, and `run_with` fails at startup when it has neither a
  default provisioner nor one given. The FFI crate now passes an empty
  kernelspec source and a provisioner that refuses in that case, so the
  server starts and lists no kernels.
- Registering an in-app kernel through the C ABI is split into #266, to
  be designed with the native kernel.
- CI's `ffi` job builds the iOS slices through the script on macOS; the
  no-kernels test (`starts_with_no_kernels_without_processes`) runs only
  with `cargo test -p ganymede-ffi --no-default-features --features
  gateway`, which CI doesn't run yet.
