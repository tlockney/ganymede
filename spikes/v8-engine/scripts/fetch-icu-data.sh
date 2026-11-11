#!/bin/sh
# The v8 crate's published source leaves out ICU's iOS data file
# (third_party/icu/ios/icudtl.dat), which a from-source build for iOS
# needs. This puts it in the crate's source in Cargo's registry, from
# ICU's revision in the crate's v8/DEPS, and checks its SHA-256.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^v8 = { version = "=\([^"]*\)".*/\1/p' Cargo.toml)
rev=d578f2e8b7bd5938e21cfb6bf15c079e0aa5b738 # ICU in v8 152.2.0's v8/DEPS
sha256=86306257583c511bdac2901c7809e89972fad4dc0ff87abee9eee74a082d3cd1

cargo fetch --locked > /dev/null
src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name "v8-$version" | head -1)
test -n "$src" || { echo "v8-$version not found in Cargo's registry" >&2; exit 1; }
grep -q "$rev" "$src/v8/DEPS" || { echo "v8-$version's DEPS no longer names ICU $rev: update this script" >&2; exit 1; }

dest="$src/third_party/icu/ios/icudtl.dat"
if [ -f "$dest" ] && shasum -a 256 "$dest" | grep -q "^$sha256 "; then
  exit 0
fi
mkdir -p "$(dirname "$dest")"
curl -fsSL "https://chromium.googlesource.com/chromium/deps/icu/+/$rev/ios/icudtl.dat?format=TEXT" | base64 -d > "$dest.tmp"
shasum -a 256 "$dest.tmp" | grep -q "^$sha256 " || { echo "icudtl.dat: unexpected SHA-256" >&2; rm -f "$dest.tmp"; exit 1; }
mv "$dest.tmp" "$dest"
echo "added $dest" >&2
