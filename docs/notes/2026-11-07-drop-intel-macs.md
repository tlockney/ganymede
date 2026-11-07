## 2026-11-07, Apple silicon only; apps on macOS 26 and iOS 26 (#454)

- Thomas decided the Apple apps require macOS 26 and iOS 26 and that the
  Mac app is arm64 only. macOS 26 is the last release Intel Macs get, and
  every Apple silicon Mac runs it.
- `scripts/build-xcframework.sh` builds the macOS slice for
  `aarch64-apple-darwin` only (no `lipo`) and defaults both deployment
  targets to 26.0. That is one of four Rust builds of ganymede-ffi gone
  from CI's `ffi` job and from each ganymede-ui app build.
- The release no longer builds `x86_64-apple-darwin` or checks it under
  Rosetta. v0.7.0's Intel archive was downloaded once.
- `install.sh` refuses on an Intel Mac and points at `cargo install`; a
  Rosetta shell on Apple silicon still gets the arm64 binary.
  `test-install.sh` checks the refusal when it runs on an Intel Mac.
- The landing page still says macOS 13 and Intel: it describes the
  latest ganymede-ui release, which does. It changes when ganymede-ui
  releases the new floor (ganymede-ui#356).
