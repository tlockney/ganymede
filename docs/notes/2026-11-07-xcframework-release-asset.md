## 2026-11-07, XCFramework as a release asset (#456)

ganymede-ui built ganymede-ffi from source at its pinned `GANYMEDE_REF`
in three jobs: 10 min of its 24 min PR job, and 10, 25 and 18 min in
the v0.5.0 release. rust-cache restores dependencies but never
workspace crates, so each run paid the full release build of a ref that
hadn't changed.

`release.yml` now has an `xcframework` job on macOS that runs
`scripts/build-xcframework.sh` and attaches `GanymedeFFI.xcframework.zip`
and its `.sha256` to the release and to `main-build`. The name has no
version, so ganymede-ui can fetch it by tag
(`releases/download/<tag>/GanymedeFFI.xcframework.zip`). The release
and main-build jobs wait for it, so the attestation and `SHA256SUMS`
cover it like the CLI archives. The CLI targets are unchanged.
ganymede-ui's side is ganymede-ui#359.

The zip carries `LICENSE` and a `THIRD-PARTY-NOTICES` from `cargo about`
for ganymede-ffi's tree on the three Apple targets, since the release
now redistributes those crates. The job skips pull requests: CI's
`ffi (macos-latest)` already runs the build script there, and a
`Cargo.toml` change would otherwise queue a second macOS build.
