## 2026-11-06, Docs-only pull requests skip the build (#418)

A pull request that changed only docs or the site ran the whole CI,
three macOS jobs included. A `changes` job now runs
`scripts/ci-changes.sh` on the PR's merge commit: if every file is
Markdown, under `docs/`, or under `site/` other than `install.sh`, the
build and test jobs don't run.

The ruleset requires 13 checks by name, 6 of them matrix cells such as
`e2e (macos-latest)`. A job skipped by its `if` reports success, but a
matrix job skipped whole reports one check without the cell's name, and
the required ones would wait forever. So the matrix jobs (`e2e`, `ffi`,
`install-script`, `workstation`) still start, on `ubuntu-latest` instead
of their `matrix.os`, with every step skipped; the check names come from
the matrix, not the runner. The other jobs are skipped by `if`.

The conditions test `output != 'false'`, so if `changes` fails, its
output is empty and everything runs. Pushes to `main`, the weekly
schedule and manual runs aren't pull requests and run everything. The
same change went into ganymede-ui (#344), cdp-kernel (#431) and
ganymede-device (#45).
