## 2026-10-18, CI hygiene (#62)

CI ran every action by a movable tag, with no `permissions:` in `ci.yml`,
no dependency audit, no MSRV, no timeouts, and the e2e pins only in the
`e2e` job: the workstation and install-script jobs took whatever
cdp-kernel npm had that day. Now:

- Actions are pinned by commit SHA with the version in a comment, and
  Dependabot bumps them, the Cargo lockfiles, and the npm packages
  weekly. Checkouts don't keep the token (`persist-credentials: false`).
- `ci.yml` is `contents: read`, PR runs cancel their older runs, and every
  job has a timeout.
- `rust-version = "1.95"` and an `msrv` job. 1.95 is not a guess:
  rustyline 19 uses `cfg_select!`, stable since 1.95, without declaring a
  `rust-version`, so the highest one in `cargo metadata` (1.89) is wrong.
  Only building with the old toolchain finds that.
- `cargo deny` for advisories, bans, and sources. Thirteen crates have
  two versions (base64, digest, getrandom, syn, thiserror...); they are
  warnings, not failures.
- One `env:` block holds the e2e pins; `test-workstation.sh` and
  `test-install.sh` read `CDP_KERNEL_VERSION` (the latter with an npm shim,
  since `site/install.sh` installs the latest, as a user gets it). A weekly
  run uses the latest releases instead.
- The `e2e` job runs `cargo test --workspace` (unit tests on macOS too)
  and fails if any test printed `skipping:`; the `rust` job lists its
  skips in the run summary. `cargo doc` with `-D warnings` found one
  broken intra-doc link.

See [development](../development.md#ci) for the rules.
