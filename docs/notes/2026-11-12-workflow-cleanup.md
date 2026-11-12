## 2026-11-12, workflow cleanup across the four repos (#736, #737)

A review of the GitHub Actions workflows in ganymede, ganymede-ui,
cdp-kernel and ganymede-device found them mostly consistent: pr-issue.yml
is identical everywhere, every CI has the same `changes` job, actions are
pinned by SHA and every job has a timeout. The changes, one PR per repo:

- **ganymede (#737):** a failed weekly CI run opens or comments on a
  "Weekly CI run failed" issue. It failed on 2026-10-26 and 2026-11-09
  and nobody was told. A pull request checks RustSec advisories only when
  it changes `Cargo.lock`, a `Cargo.toml` or `deny.toml`
  (`ci-changes.sh`'s new `deps` output); bans and sources run on every
  code PR, and pushes to main and the weekly run check advisories every
  time. The `audit` job keeps its name, so the ruleset is unchanged.
- **cdp-kernel (#473) and ganymede-device (#130):** a new push cancels a
  PR's older CI run, as ganymede and ganymede-ui already did. Without it,
  the old run kept a macOS runner the four repos share. Checkouts set
  `persist-credentials: false`, except cdp-kernel's tag job, which pushes.
  npm and Playwright browsers are cached.
- **ganymede-ui (#547):** CI no longer builds an unsigned Release app on
  pushes to main, since main-build.yml publishes the signed one, and the
  Apple setup five jobs copied is one composite action.
- **Names:** Title case (CI, Pages, Publish, ...). Only `name:` changed;
  file names stay, because npm and JSR trusted publishing and scripts
  refer to workflows by path, and required checks use job names.
  ganymede's tag-release.yml is "Tag release (recovery)", since
  ganymede-ui's Tag release runs on every release and this one never
  does on its own.

"Review gate" still shows in each repo's Actions sidebar although no repo
has the file: GitHub lists a workflow while any of its runs exist.
Deleting its runs removes it.
