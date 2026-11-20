#!/usr/bin/env bash
# Builds ganymede-ffi for macOS (Apple silicon), iOS, and the iOS
# simulator, and packages them with include/ganymede.h as
# target/xcframework/GanymedeFFI.xcframework, which the Swift shell links.
# The iOS slices build without `zmq` and `subprocess`: an iOS app can't
# start kernel processes, so that server lists no kernels unless the
# config sets `gateway_url`. All three slices build with `v8-bench`, the
# engine benchmark behind the apps' Settings (#892): rusty_v8 downloads its
# prebuilt static V8 for each target, checked against the SHA-256 in
# scripts/v8-archive-sha256.sh. The iOS device one is built without a JIT
# (iOS apps have none); the simulator's keeps it. It lives in this library because a second Rust static
# library in the app would clash with this one's runtime symbols. Default
# builds (no feature) neither download nor link V8. The apps need macOS 26
# or iOS 26, and the Mac app is arm64 only (#454). Needs macOS with Xcode.
set -euo pipefail
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-26.0}
export IPHONEOS_DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-26.0}
# Line tables in the static library: the app's link copies them into its
# dSYM, so crash reports show Rust functions, files and lines
# (ganymede-ui#318). The app's own release build strips its binary.
export CARGO_PROFILE_RELEASE_DEBUG=${CARGO_PROFILE_RELEASE_DEBUG:-line-tables-only}
export CARGO_PROFILE_RELEASE_STRIP=${CARGO_PROFILE_RELEASE_STRIP:-none}
build() {
  local target=$1
  shift
  RUSTY_V8_ARCHIVE_SHA256=$(scripts/v8-archive-sha256.sh "$target") \
    cargo build --locked --release -p ganymede-ffi --target "$target" "$@"
}
build aarch64-apple-darwin --features v8-bench
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  build "$t" --no-default-features --features gateway,v8-bench
done
lib=libganymede_ffi.a
out=target/xcframework
# Each slice has V8 in it: a build that dropped the feature would still
# assemble and fail only when the app links the benchmark.
for t in aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim; do
  # grep reads all of nm's output: with -q it exits at the first match, nm
  # gets SIGPIPE, and pipefail fails the pipeline although V8 is there.
  if ! nm -g "target/$t/release/$lib" 2> /dev/null | grep 'v8__V8__Initialize' > /dev/null; then
    echo "$t: $lib has no V8 symbols (v8__V8__Initialize)" >&2
    exit 1
  fi
done
rm -rf "$out"
mkdir -p "$out/macos" "$out/headers"
cp "target/aarch64-apple-darwin/release/$lib" "$out/macos/$lib"
cp crates/ganymede-ffi/include/ganymede.h "$out/headers/"
cat > "$out/headers/module.modulemap" <<'MAP'
module GanymedeFFI {
    header "ganymede.h"
    export *
}
MAP
xcodebuild -create-xcframework \
  -library "$out/macos/$lib" -headers "$out/headers" \
  -library "target/aarch64-apple-ios/release/$lib" -headers "$out/headers" \
  -library "target/aarch64-apple-ios-sim/release/$lib" -headers "$out/headers" \
  -output "$out/GanymedeFFI.xcframework"
(cd "$out" && ditto -c -k --keepParent GanymedeFFI.xcframework GanymedeFFI.xcframework.zip)
echo "$out/GanymedeFFI.xcframework.zip"
