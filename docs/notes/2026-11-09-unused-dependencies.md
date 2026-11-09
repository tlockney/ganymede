## 2026-11-09, unused dependencies, dead helpers, iOS build warnings (#625)

A read of the whole project for things that are broken or no longer
needed found little in this repo that tools flag: clippy, the tests, the
docs build and the links are clean. What it did find:

- **Dependencies nothing uses.** A script compared each crate's
  `Cargo.toml` with the identifiers in its sources. Removed:
  `ganymede-protocol` from ganymede-api and ganymede-exec, `serde` from
  ganymede-gateway, `thiserror` from ganymede-server, the dev-dependencies
  `tempfile` and `anyhow` from ganymede-engine and `anyhow` from
  ganymede-kernel-host, and `chrono` from ganymede-kernel-host's
  browser-wasm section. That last one only turned on chrono's `wasmbind`,
  which ganymede-protocol already does for the whole build; kernel-host
  has no chrono dependency otherwise. rustc doesn't warn about an unused
  dependency, and cargo-machete or cargo-udeps isn't in CI.
- **`e2e/wasm-smoke/Cargo.lock` was three releases behind.** It listed
  the workspace crates at 0.4.0 and lacked ganymede-nbformat's `base64`
  dependency. The `portability` job builds it without `--locked`, so
  cargo quietly fixed it up in each run. The file is refreshed here; it
  will drift again at the next version bump, since `scripts/release.sh`
  doesn't touch it. Building with `--locked` would catch that, and would
  also fail every release pull request until the release hook updates
  this lockfile too.
- **Dead helpers.** `testing.rs` in ganymede-kernel-host kept an unused
  `mpsc` import alive with an `_unused` function under
  `#[allow(dead_code)]`; both are gone. `KERNELS` in ganymede-cli's test
  helpers had no users.
- **iOS and wasm warnings.** With Rust 1.97, `cargo check -p
  ganymede-terminals --target aarch64-apple-ios` printed 8 warnings: the
  stub `pty::Waiter` was an empty enum, so rustc reported everything after
  `process.waiter()` in `Terminal::start` as unreachable and the variables
  before it as unused. The stub is now a unit struct. Nothing can make
  one, since the stub `Process` is still an empty enum. The `portability`
  job uses `cargo check`, which passes with warnings, so these never
  failed CI.
- **`.dockerignore`.** `node_modules` matches only the repository root.
  A local `docker build -f container/Dockerfile .` sent
  `docs/node_modules` and `validation/node_modules` to the daemon. It is
  `**/node_modules` now. CI's checkouts have neither directory.
- **`pages.yml`.** The comment about the cell API reference sat above the
  `npm test` step.

Checked locally: `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, the Windows clippy run, and the
`portability` job's checks and tests for iOS, Android and both wasm
targets.
