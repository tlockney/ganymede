#!/bin/sh
# What V8 adds to an iOS app binary (#646). On an arm64 Mac with Xcode:
#
#   V8_FROM_SOURCE=1 scripts/ios-size.sh
#
# Builds the spike's static library for aarch64-apple-ios twice, with V8
# (rusty_v8 from source: jitless, no WebAssembly, as its build.rs sets for
# the device) and without, links each into the same stand-in app
# (size-main.c) with dead-stripping, strips it as an App Store build is
# stripped, and prints the sizes, raw and compressed, as JSON. The first
# build of V8 from source takes a long time; later ones reuse target/.
#
# Leaves target/aarch64-apple-ios/release/libganymede_v8_spike.a, which
# the bench app (ios/) links.
set -eu
cd "$(dirname "$0")/.."

target=aarch64-apple-ios
out=target/ios-size
mkdir -p "$out"

# --print native-static-libs: what else the library needs at link time.
libs_of() {
  sed -n 's/.*native-static-libs: //p' "$1" | tail -1
}

if [ -n "${V8_FROM_SOURCE:-}" ]; then
  scripts/fetch-icu-data.sh
fi

echo "building with V8 ..." >&2
cargo rustc --release --locked --target "$target" --lib --crate-type staticlib -- --print native-static-libs 2> "$out/with-v8.log" || {
  tail -50 "$out/with-v8.log" >&2
  exit 1
}
cp "target/$target/release/libganymede_v8_spike.a" "$out/with-v8.a"

echo "building without V8 ..." >&2
cargo rustc --release --locked --target "$target" --lib --crate-type staticlib --no-default-features \
  --target-dir target/baseline -- --print native-static-libs 2> "$out/without-v8.log" || {
  tail -50 "$out/without-v8.log" >&2
  exit 1
}
cp "target/baseline/$target/release/libganymede_v8_spike.a" "$out/without-v8.a"

sdk=$(xcrun --sdk iphoneos --show-sdk-path)
for v in with-v8 without-v8; do
  # shellcheck disable=SC2046 # the library list splits into arguments
  xcrun --sdk iphoneos clang -target arm64-apple-ios26.0 -isysroot "$sdk" -Os -dead_strip \
    scripts/size-main.c "$out/$v.a" $(libs_of "$out/$v.log") -o "$out/$v"
  xcrun strip "$out/$v"
done

bytes() { stat -f %z "$1"; }
gz() { gzip -9 -c "$1" | wc -c | tr -d ' '; }
with=$(bytes "$out/with-v8")
without=$(bytes "$out/without-v8")
with_gz=$(gz "$out/with-v8")
without_gz=$(gz "$out/without-v8")
cat <<JSON
{
  "target": "$target",
  "v8_crate": "$(sed -n 's/^v8 = { version = "=\([^"]*\)".*/\1/p' Cargo.toml)",
  "static_lib_bytes": $(bytes "$out/with-v8.a"),
  "app_binary_with_v8_bytes": $with,
  "app_binary_without_v8_bytes": $without,
  "added_bytes": $((with - without)),
  "added_gzip_bytes": $((with_gz - without_gz)),
  "link_libs_with_v8": "$(libs_of "$out/with-v8.log")"
}
JSON
