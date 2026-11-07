## 2026-11-07, one Linux release build per CI run (#460)

lsp, workstation (ubuntu) and image each compiled `ganymede-cli` in
release mode: about 2 min each in the first two, and 4-5 min inside
Docker for image, whose BuildKit cache mounts don't survive between
hosted runners. A `linux-release` job now builds it once and uploads it
for a day; the three download it (artifacts drop the executable bit, so
each `chmod +x`es it). The Dockerfile gained `GANYMEDE_SOURCE=local`,
which copies `container/bin/ganymede` from the build context.
`workstation (macos-latest)` still builds its own.

e2e (ubuntu) stays separate from coverage, though both run the suite
with the same kernels: e2e is a required check and fails on a skipped
test, coverage is informational and instrumented.
