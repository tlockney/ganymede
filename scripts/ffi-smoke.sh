#!/usr/bin/env bash
# Builds ganymede-ffi as a static library and links tests/smoke.c against
# it through include/ganymede.h, so a header that drifts from the Rust
# functions fails to link. Runs the result: start, print the URL, stop.
set -euo pipefail
cd "$(dirname "$0")/.."
crate=crates/ganymede-ffi
libs=$(cargo rustc --color never --locked -q -p ganymede-ffi --release --crate-type staticlib -- --print native-static-libs 2>&1 |
  sed -n 's/.*native-static-libs: //p' | tail -1)
out=$(mktemp -d)
cc -Wall -Werror -I "$crate/include" "$crate/tests/smoke.c" target/release/libganymede_ffi.a $libs -o "$out/smoke"
(cd "$out" && ./smoke) | grep -E '^http://127\.0\.0\.1:[0-9]+/\?token=[0-9a-f]+$'
echo "ganymede-ffi smoke test passed"
