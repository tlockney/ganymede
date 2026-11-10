## 2026-11-09, portability job fails on warnings and checks wasm-smoke's lockfile (#636)

Items 14 and 15 of the project read of 2026-11-09 (#629). Both are about
things CI built without looking at.

- **Warnings.** The `portability` job is the only place the core
  libraries are compiled for iOS, Android and wasm. Clippy runs with
  `-D warnings`, but for the desktop targets, so a warning that appears
  only on the others showed in a log nobody reads: `ganymede-terminals`
  printed 8 on iOS and wasm until #626. The job now sets
  `RUSTFLAGS=-D warnings`. Cargo caps lints for registry dependencies,
  so this covers the workspace's own crates and `e2e/wasm-smoke`, not
  theirs.
- **The lockfile.** `e2e/wasm-smoke` is its own workspace, so its
  `Cargo.lock` is tracked but read by one CI step only, and that step
  built without `--locked`: cargo rewrote the stale file in each run and
  threw the result away. It went stale at every release, because
  cargo-release bumps the workspace and knows nothing of this file, and
  whenever a crate it depends on gained a dependency. The step now
  checks the lockfile first (`cargo metadata --locked`) and fails with
  the command that refreshes it, then builds `--locked`.
  `scripts/release.sh` runs that command after cargo-release's commit
  and amends the lockfile into it.
- `cargo update --workspace` is the refresh that changes least: it
  updates the path dependencies' versions and adds what is missing, and
  leaves every registry dependency where it was. Simulating a bump to
  0.7.1 changed five lines, one per crate.

The cost: a PR that adds a dependency to a crate the engine needs, or
that raises a version requirement past what this lockfile holds, now
fails `portability` until the lockfile is refreshed. That includes a
Dependabot PR for the main workspace that raises a requirement in a
manifest. The error names the command.

Two things the review of #637 caught. The refresh in `release.sh` runs
after cargo-release has committed on `main`, so a failed `cargo update`
left that commit there with no release branch, and a rerun stopped on
"HEAD is not origin/main". The script now undoes the commit when the
refresh fails. And the CI step called every `cargo metadata --locked`
failure a stale lockfile; it prints cargo's error and adds the
annotation only when cargo says `--locked` stopped it.
