## 2026-11-07, a lighter XCFramework check in CI (#457)

`ffi (macos-latest)` was the longest CI job (13-14 min) and set a PR's
wall time: `scripts/build-xcframework.sh` built every slice with the
release profile (thin LTO, one codegen unit), 9.6-9.8 min.

Both `ffi` cells now build with `CARGO_PROFILE_RELEASE_LTO=off` and 16
codegen units, on PRs and on main alike, so a PR restores main's cache
for the same settings. Building differently on PRs would miss main's
cache: the profile is part of every dependency's fingerprint. The full
release build moved to `release.yml`, which publishes the framework on
every push to main and every tag (#456). The weekly scheduled run keeps
the release profile, so drift under LTO shows up even in a week with no
push to main.

`scripts/ci-changes.sh` also prints `ffi`: false when only code outside
ganymede-ffi's dependency tree changed (ganymede-cli, ganymede-engine,
e2e, evals, plugins, validation, container, other scripts and
workflows). Then the macOS cell runs its one step on an ubuntu runner
and frees the macOS runner. The ubuntu cell checks with
`cargo tree --target all` that ganymede-ffi still doesn't depend on
ganymede-cli or ganymede-engine, so the list can't go stale silently.
