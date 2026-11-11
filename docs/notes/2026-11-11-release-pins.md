## 2026-11-11, releases pick up the latest dependencies (#673)

A release round was seven pull requests in six serial steps, three of
them only moving a pin: cdp-kernel into Ganymede, the ganymede-ui
release into `ganymede ui install`, and Ganymede back into ganymede-ui.
Each waited for CI, the reviewer and a merge. The order is now
cdp-kernel, Ganymede, ganymede-ui, and `scripts/release.sh` writes the
latest cdp-kernel and ganymede-ui releases into the release commit
itself through `scripts/release-pins.sh`, so the release pull request's
CI tests them.

`ui install`'s default keeps its built-in SHA-256: the script takes the
release's `SHA256SUMS` line for the zip and checks it against the zip it
downloads, so a mismatch stops the release. The cost Thomas accepted is
that the default trails ganymede-ui by one release, since the apps now
release after Ganymede.

The script edits each pin by a fixed string and fails unless exactly one
line holds it, so a reworded doc fails loudly instead of being skipped.
It fails too when a latest release is older than the pin on main.

The script does every lookup, download and hash check before it writes,
rejects arguments other than `--check`, and counts a prerelease as older
than its own release (`sort -V` alone ranks `0.11.0` below
`0.11.0-rc.1`). The same fixes came out of the review of ganymede-ui#490.

After review: a write run first does every replacement in check mode,
so a line it can't find stops it before anything changes, and a missing
line now prints which one. The pins it moved also go into the version's
CHANGELOG.md section as "Pick up ..." lines, so the GitHub Release notes
list them; git-cliff skips the release commit, so a later regeneration
of CHANGELOG.md drops those lines from the file, but the published
notes keep them. curl retries, and GitHub API calls use `GH_TOKEN` when
it is set.
