## 2026-11-07, Docs-only pushes to main skip CI (#458)

`scripts/ci-changes.sh` ran everything for any event but a pull request,
so merging a docs-only PR still ran the whole CI on main: the merges of
#449, #451 and #453 took 16-22 min each, four macOS jobs included. A
push now diffs against `github.event.before`, the commit main was at
before it, with the same rules as a PR. The script fetches that commit
when the shallow checkout lacks it, and runs everything when the base is
all zeros, can't be fetched, or can't be diffed. The schedule and manual
runs still run everything; release.yml already skipped these pushes with
`paths-ignore`.
