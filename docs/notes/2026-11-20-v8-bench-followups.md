## 2026-11-20, v8-bench review follow-ups (#892)

Two leftovers from the review of #895. `scripts/test-ci-changes.sh` gains
a case for `scripts/v8-archive-sha256.sh` on its own: in the shared "ffi
scripts" case the other files set `ffi=true` anyway, so reverting that
file's routing failed nothing. The ganymede-ffi comments now say the
benchmark is for the apps to run on a device, as docs/embedding.md does,
instead of naming a Settings screen the apps haven't released.
