#!/usr/bin/env bash
# Builds ganymede-ffi for macOS (arm64 + x86_64), iOS, and the iOS
# simulator, and packages them with include/ganymede.h as
# target/xcframework/GanymedeFFI.xcframework, which the Swift shell links.
# The iOS slices build without `zmq` and `subprocess`: an iOS app can't
# start kernel processes, so that server lists no kernels unless the
# config sets `gateway_url`. Needs macOS with Xcode.
set -euo pipefail
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
export IPHONEOS_DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-16.0}
for t in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --locked --release -p ganymede-ffi --target "$t"
done
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo build --locked --release -p ganymede-ffi --target "$t" --no-default-features --features gateway
done
lib=libganymede_ffi.a
out=target/xcframework
rm -rf "$out"
mkdir -p "$out/macos" "$out/headers"
lipo -create "target/aarch64-apple-darwin/release/$lib" "target/x86_64-apple-darwin/release/$lib" \
  -output "$out/macos/$lib"
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
