## 2026-11-07, one Linux release build per CI run (#460)

lsp, workstation (ubuntu) and image each compiled `ganymede-cli` in
release mode: about 2 min each in the first two, and 4-5 min inside
Docker for image, whose BuildKit cache mounts don't survive between
hosted runners. A `linux-release` job now builds it once and uploads it
for a day; the three download it (artifacts drop the executable bit, so
each `chmod +x`es it). The Dockerfile gained `GANYMEDE_SOURCE=local`,
which copies `container/bin/ganymede` from the build context.
`workstation (macos-latest)` still builds its own.

`linux-release` runs in the `rust:1.95-trixie` container, the
Dockerfile's build image. Built on the ubuntu runner, the binary needed
glibc 2.43 and failed to start in the trixie image (2.41); built on
trixie, it runs in the image and on the newer runner.

`needs` covers a whole matrix, so `workstation (macos-latest)` waited
for linux-release before queueing for a macOS runner, though it doesn't
use the binary. It is now its own job, `workstation-macos`, with the
same check name and the same steps through a YAML anchor. Without the
Rust toolchain step, the ubuntu cell's `cargo install nb-cli` on a cache
miss (always on the weekly run) would have used the runner image's
Rust; it sets up stable for that step.

e2e (ubuntu) stays separate from coverage, though both run the suite
with the same kernels: e2e is a required check and fails on a skipped
test, coverage is informational and instrumented.
