## 2026-11-07, Apple apps on Apple silicon, macOS 26 and iOS 26 (#454)

- Thomas decided the Apple apps require macOS 26 and iOS 26 and that the
  Mac app is arm64 only. macOS 26 is the last release Intel Macs get, and
  every Apple silicon Mac runs it.
- The server and CLI keep broad support (Thomas): the release still
  builds `x86_64-apple-darwin`, and `install.sh` still installs it on
  Intel Macs. Only the XCFramework the apps link changes.
- `scripts/build-xcframework.sh` builds the macOS slice for
  `aarch64-apple-darwin` only (no `lipo`) and defaults both deployment
  targets to 26.0: one of four Rust builds of ganymede-ffi gone from CI's
  `ffi` job and from each ganymede-ui app build.
- The landing page still says macOS 13 and Intel for the apps: it
  describes the latest ganymede-ui release, which does. It changes when
  ganymede-ui releases the new floor (ganymede-ui#356).
