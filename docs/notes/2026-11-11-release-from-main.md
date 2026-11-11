## 2026-11-11, publish from the release commit's main build (#675)

Merging a release PR built its commit twice: once on the main push as a
`main-build`, then again on the tag `tag-release.yml` pushed, each time
five targets and the XCFramework, all on the same scarce macOS runners.
Now the main push's `version` job does `tag-release.yml`'s check (a
version with a CHANGELOG.md section and no tag), and if it holds, that
run is the release: it builds as `X.Y.Z`, tags the commit as it
publishes, and skips `main-build`.

Two things change with it. A release build must not be cancelled by the
next push, so release runs leave the shared `main` concurrency group.
And only the commit that changed the version is a release run (it
compares `HEAD^`'s workspace version): a merge that lands before the
release publishes must not ship under notes that don't list it, nor
drop out of the next release's notes. A failed release is rerun, or
released from `tag-release.yml` by hand. The publish step makes the tag
only if no other run has, and otherwise does nothing (the image too),
so two runs can't publish the version twice. `tag-release.yml`
stays, by hand only, to release from a tag when a run can't be rerun.

The release commit is the push that changed the version, compared with
the commit `main` was at before the push (`github.event.before`), so a
release PR merged by rebase with several commits still counts.
`tag-release.yml` tags that commit, found from history, or a commit you
give it, not `main`'s head, so a recovery doesn't pull later merges
into the release.
