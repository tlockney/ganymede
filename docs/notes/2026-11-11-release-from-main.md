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
And until the tag exists every push to main is a release run, so a
failed release is retried by the next merge; the publish step makes
the tag only if no other run has, and otherwise does nothing (the image
too), so two runs can't publish the version twice. `tag-release.yml`
stays, by hand only, to release from a tag when a run can't be rerun.
