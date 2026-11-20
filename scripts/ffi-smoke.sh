#!/usr/bin/env bash
# Builds ganymede-ffi as a static library and links tests/smoke.c against
# it through include/ganymede.h, so a header that drifts from the Rust
# functions fails to link. Runs the result: start, print the URL, stop.
# FFI_FEATURES (cargo features, comma separated) builds with them, such as
# v8-bench, whose functions are exported with and without the feature; the
# test then also expects ganymede_v8_bench_available() to be 1. A build
# with v8-bench downloads rusty_v8's library, pinned to the SHA-256 that
# scripts/v8-archive-sha256.sh has for this host's target.
set -euo pipefail
cd "$(dirname "$0")/.."
crate=crates/ganymede-ffi
case ,${FFI_FEATURES:-}, in
  *,v8-bench,*) RUSTY_V8_ARCHIVE_SHA256=$(scripts/v8-archive-sha256.sh "$(rustc -vV | sed -n 's/^host: //p')") && export RUSTY_V8_ARCHIVE_SHA256 ;;
esac
libs=$(cargo rustc --color never --locked -q -p ganymede-ffi --release ${FFI_FEATURES:+--features "$FFI_FEATURES"} --crate-type staticlib -- --print native-static-libs 2>&1 |
  sed -n 's/.*native-static-libs: //p' | tail -1)
out=$(mktemp -d)
case ,${FFI_FEATURES:-}, in *,v8-bench,*) expect=-DEXPECT_V8_BENCH ;; *) expect= ;; esac
cc -Wall -Werror $expect -I "$crate/include" "$crate/tests/smoke.c" target/release/libganymede_ffi.a $libs -o "$out/smoke"
(cd "$out" && ./smoke) | grep -E '^http://127\.0\.0\.1:[0-9]+/\?token=[0-9a-f]+$'
echo "ganymede-ffi smoke test passed"
