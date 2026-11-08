## 2026-11-08, release.sh and tag-release resume after a partial failure (#572)

`scripts/release.sh --execute` run from a `release/vX.Y.Z` branch whose
head is "Release X.Y.Z" now resumes: it skips the bump, pushes, reuses an
open "Release vX.Y.Z" issue, and stops if the PR is already open. The
bump steps moved into a `prepare` function so the resume check reads
first. If the branch already exists when a fresh run commits, the run
undoes its own commit (`git reset --keep HEAD~1`) and says to resume
from the branch instead.

`tag-release.yml` used to exit as soon as the tag existed, so a run that
pushed the tag but failed to dispatch `release.yml` could never be
finished by a rerun. It now dispatches when the tag is on this commit and
there is neither a release nor a `release.yml` run for the tag.

Tested the resume path in a scratch repo with stub `cargo-release`,
`git-cliff` and `gh`: first rerun pushes and opens the issue and PR, a
second exits on the open PR, and with only the issue left it reuses it.
