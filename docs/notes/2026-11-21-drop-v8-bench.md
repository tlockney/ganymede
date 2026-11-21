## 2026-11-21, Drop the V8 benchmark from ganymede-ffi (ganymede-ui#765)

The in-app TypeScript kernel stays on the WebKit Worker (decided
2026-11-20). V8 came into the app only so the apps could run the engine
benchmark on a device (#892), and with the engine decided, it comes back
out. The device runs in #646 measured jitless V8 2 to 8x slower than the
Worker on both the Mac and the iPhone. It added about 7 MB to the
download, kept about 166 MB of footprint after a run, and needed the
extended virtual addressing entitlement. The spike stays in `spikes/v8-engine/`
with its workflow `v8-spike.yml`, for when the question comes back.

Removed:

- **`ganymede-ffi`**: the `v8-bench` feature, `src/v8_bench.rs`, and the
  C functions `ganymede_v8_bench_available`, `ganymede_v8_bench_run` and
  `ganymede_v8_bench_free`, with their text in `ganymede.h`. The `v8`,
  `serde` and `libc` dependencies were optional and only the feature used
  them, so they leave `Cargo.toml`; `Cargo.lock` loses `v8` and what only
  it pulled in. `tests/smoke.c` no longer calls the three functions, and
  `scripts/ffi-smoke.sh` loses its `FFI_FEATURES` hook, which only
  the benchmark used.
- **Build**: `scripts/build-xcframework.sh` builds the three slices with
  their default feature sets again (the iOS ones with `--no-default-features
  --features gateway`), downloads no V8, and no longer checks each library
  for V8 symbols. `scripts/v8-archive-sha256.sh`, which pinned rusty_v8's
  library hashes for it, is deleted: the spike workflow doesn't use it.
  `release.yml`'s `cargo about` runs without the feature, and
  `crates/ganymede-ffi/V8-NOTICES.txt`, V8's bundled-code licenses that
  were appended to the XCFramework's notices, is deleted with it.
- **CI**: the `ffi` job's V8 step (clippy, tests and the smoke test with
  `v8-bench`) is gone, the job's comment no longer says the framework is
  built with V8, and `ci-changes.sh` and its test lose the V8 archive
  script's case.
- **`deny.toml`**: the RUSTSEC-2024-0436 ignore for `paste`, which only
  `v8` brought in.
- **Docs**: `docs/embedding.md` loses the three functions and the
  feature; its Unreleased note says the latest release still has them.
  The roadmap says the in-app kernel stays on the Worker.

`v0.15.0` still has the functions. ganymede-ui's Swift calls them, so
ganymede-ui must drop its benchmark in the same PR that moves
`GANYMEDE_REF` to a release without them: the app fails to link against
the new XCFramework otherwise. The release itself is not part of this
change.
