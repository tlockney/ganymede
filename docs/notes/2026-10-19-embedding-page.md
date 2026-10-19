## 2026-10-19, an embedding page (#189)

- Every example on the page was compiled and run outside the repo
  against these crates by path: the streaming example on the built-in
  fake kernel, an engine with `default-features = false` and its own
  provisioner and transport factory (no `zeromq` in `cargo tree`), the
  gateway snippet (compiles; stops at `refresh()` with no server), and
  the C call sequence with `cc -Wall -Werror` against `ganymede.h` and
  the staticlib on Linux. The Swift snippet and
  `build-xcframework.sh` need macOS and were not run.
- `ganymede-ffi` builds the server with default features, so its iOS
  slice still has `zmq`, `subprocess` and `gateway`, and the C API can't
  pass an app's own kernel. The ffi dev log entry said #160 removed
  them; it didn't. Filed as #208.
- `Handler::on_input` became async after v0.3.0, so the streaming
  section carries an Unreleased note while the engine table above it,
  all in v0.3.0, doesn't.
