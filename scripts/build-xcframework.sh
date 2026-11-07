#!/usr/bin/env bash
# Builds ganymede-ffi for macOS (Apple silicon), iOS, and the iOS
# simulator, and packages them with include/ganymede.h as
# target/xcframework/GanymedeFFI.xcframework, which the Swift shell links.
# The iOS slices build without `zmq` and `subprocess`: an iOS app can't
# start kernel processes, so that server lists no kernels unless the
# config sets `gateway_url`. The apps need macOS 26 or iOS 26, and the Mac
# app is arm64 only (#454). Needs macOS with Xcode.
set -euo pipefail
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-26.0}
export IPHONEOS_DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-26.0}
# Line tables in the static library: the app's link copies them into its
# dSYM, so crash reports show Rust functions, files and lines
# (ganymede-ui#318). The app's own release build strips its binary.
export CARGO_PROFILE_RELEASE_DEBUG=${CARGO_PROFILE_RELEASE_DEBUG:-line-tables-only}
export CARGO_PROFILE_RELEASE_STRIP=${CARGO_PROFILE_RELEASE_STRIP:-none}
cargo build --locked --release -p ganymede-ffi --target aarch64-apple-darwin
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo build --locked --release -p ganymede-ffi --target "$t" --no-default-features --features gateway
done
lib=libganymede_ffi.a
out=target/xcframework
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
