## 2026-11-05, crash reporting in the server and CLI (#386)

`ganymede` can send panics and `error!` events from its own crates to
Sentry, behind the `crash-reporting` cargo feature, which release builds
turn on. It is off at run time unless `crash_reporting = true` is in
`ganymede.toml` or `GANYMEDE_CRASH_REPORTING=1` is set; the env var wins.
Off, sentry is never initialized.

- `crash::enabled::scrub` keeps only what docs/privacy.md lists: the
  error's first line (home rewritten to `~`, 200 characters at most) and
  stack frames, the release, and the OS, device and Rust contexts.
  Breadcrumbs, tags, extra data, user, host name and frame variables are
  dropped.
- `init_logging_with` takes an extra tracing layer so the Sentry layer
  sits beside the fmt layer; the log filter applies to the fmt layer only.
- sentry's ureq transport has no timeouts by default and its drop joins
  the sender thread, so an unanswered request would hang exit. The agent
  now has a 5 s connect and 10 s overall timeout.
- The release workflow keeps line tables, uploads debug files with a
  pinned `@sentry/cli` when `SENTRY_AUTH_TOKEN` is set, then strips.
- `ganymede debug-panic` (hidden, debug builds only) panics on purpose to
  check reporting end to end.
- The container image is built without the feature.
- When opted in, `main` sets `CDP_KERNEL_CRASH_REPORTS=1` before any thread starts, so kernels the server launches inherit it; cdp-kernel#423 reports only then.
- Review fix: sentry's `debug-images` feature sends the loaded images (debug id, load address; paths scrubbed) so Sentry can symbolicate frames from the stripped release binary against the uploaded debug files. CI's rust job now runs the crash tests and clippy with the feature. `crash::maybe_start` holds main's opt-in gate so the test covers it.
