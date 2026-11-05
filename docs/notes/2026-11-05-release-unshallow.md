## 2026-11-05, release.sh unshallows before git-cliff (ganymede-ui#179)

git-cliff rebuilds the whole of `CHANGELOG.md` from history. In a shallow
clone (the default in cloud sessions and many CI checkouts) the merges of
older releases are missing, and their entries dropped out of the 0.3.0
changelog without an error. `scripts/release.sh` now checks
`git rev-parse --is-shallow-repository` and runs
`git fetch --unshallow --tags origin main` first. It fetches `main` by
name, since a clone made for one branch has a refspec for that branch
only. ganymede and ganymede-ui share the guard.
