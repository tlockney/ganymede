## 2026-11-03, install the latest main build (#343)

Thomas asked for `install.sh` to install the latest build of `main`, for
quick tests without a source checkout. Only the core binary; the apps
come later and don't use install.sh.

Main builds weren't downloadable: release.yml built binaries on PRs and
manual runs only as workflow artifacts, which need a GitHub login to
download. The simplest public place is a rolling prerelease:

- release.yml now also runs on pushes to `main` (docs-only pushes
  skipped, newer pushes cancel older ones). Its `main-build` job uploads
  the archives to the `main-build` prerelease with `--clobber`, so the
  download URL is fixed. `--clobber` deletes each asset before uploading
  its replacement, so a download during an upload can 404 or fail its
  checksum; with `--main`, install.sh's errors say to retry. Builds on
  main cancel older ones per target (job-level concurrency), but the
  upload job queues instead, so a cancel never leaves the release half
  updated.
- Prereleases are skipped by `/releases/latest`, so the plain install
  is unchanged. `--latest=false` makes that explicit.
- Deleting and recreating the release each time would move the tag
  cleanly but leave a window with no files to download. So the release
  stays and the tag is moved with a ref PATCH. `GITHUB_TOKEN` has no
  `workflows` permission, and GitHub can refuse a ref update across commits that change workflow files, so the
  PATCH is best effort: on failure the run warns and the tag lags, while
  the notes and attestation name the real commit.
- `ganymede --version` on a main build is `X.Y.Z+main.<commit>`: build.rs
  appends `+$GANYMEDE_VERSION_BUILD` when it's set. Without it the
  version is unchanged, so scripts and tests that parse `--version` still
  work, and the main build doesn't claim to be release X.Y.Z.
