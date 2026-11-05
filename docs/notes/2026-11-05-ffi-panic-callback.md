## 2026-11-05, ffi panic callback for the apps' crash reports (#389)

For ganymede-ui#318 (Sentry in the Apple apps, #384).

- `ganymede_set_panic_callback` installs a panic hook once (keeping the
  default hook first) and calls the app's C callback on the panicking
  thread. The app captures an event there, and its stack capture walks
  through the Rust frames, which the app's dSYM symbolicates. A panic in
  a tokio task is caught by tokio and the app runs on, so without the
  hook nobody would see it.
- The callback is kept as an address in an `AtomicUsize`, so it can be
  swapped or removed without a lock in the hook.
- `ganymede_debug_panic` is `extern "C"`: a panic can't unwind out of
  it, so after the hook runs the process aborts, which the app's crash
  handler reports as a crash. The apps' Settings use it to check that
  Rust frames arrive symbolicated.
- No reporting code or dependency in Rust: the app owns the opt-in and
  the sending. The `crash-reporting` cargo feature (#386) is for the CLI
  and server.
- `scripts/build-xcframework.sh` builds with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and no strip, so the
  static library carries line tables into the app's dSYM.
