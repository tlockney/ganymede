## 2026-11-16, macOS CI cells skip unrelated PRs (#841)

The slow tail of PR CI is waiting for a free macOS runner: in the 50
slowest PR runs of 2026-11-07 to 2026-11-16 the last job to finish was a
macOS one, which waited 15-40 minutes and then ran 4-16. `ffi` was
already skipped by its own output (#457); `e2e (macos-latest)` and
`workstation (macos-latest)` ran on every PR with code in it.

`scripts/ci-changes.sh` now prints `macos`. It is false when every
changed file is either not code (as before) or code that neither macOS
cell builds, runs or reads. When it is false the two cells run on
`ubuntu-latest` with their steps skipped, under the same required check
names, the way `ffi` does (a job-level `BUILD` flag that every step tests).

What counts as `macos=true`: `crates/`, the Cargo and toolchain files,
`site/install.sh`, `e2e/` (ganymede-cli's tests run `e2e/collab` and
`e2e/notebooks`), `validation/` (the workstation job's Playwright and
nb-cli checks), `docs/reference.md` and `docs/mcp-tools.md` (crate tests
read them), `scripts/test-install.sh` and `scripts/test-workstation.sh`,
`.github/workflows/ci.yml`, and any path the script doesn't know.
`macos=false` for code that only Linux jobs use: `evals/`, `plugins/`,
`container/`, the other scripts and workflows, and the rest of
`.github/`. Pushes to `main`, the weekly schedule and manual runs still
run everything. `scripts/test-ci-changes.sh` runs the real script over
scratch repositories for each class of path and for each event; it runs
in the `install-script` job.

Expected effect, from the changed-file lists of the 283 commits on
`main` since 2026-11-02 (`git log --first-parent`, each classified with
the script): 233 have `code=true`, and of those 14 (6%) have
`macos=false`. Most code PRs touch `crates/`, so this frees few macOS
runner slots: about one PR in 17 with code. The 14 include a Docker bump,
journal plugin changes, and release workflow and script changes.
A larger step, running the macOS `e2e` cell only on pushes to `main` and
on the schedule, is a separate decision.
