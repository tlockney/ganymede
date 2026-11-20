#!/usr/bin/env bash
# Prints the SHA-256 of rusty_v8's prebuilt static library (the .gz release
# asset) for a target triple, to pass as RUSTY_V8_ARCHIVE_SHA256: the v8
# crate's build script downloads that archive over HTTPS and links it into
# ganymede-ffi's `v8-bench` builds, and with the variable set it fails on
# any other bytes (#892). One archive per build, so set it per target.
#
# To update with the v8 version in Cargo.lock: change VERSION and the
# hashes, which are `sha256sum` of
# https://github.com/denoland/rusty_v8/releases/download/v$VERSION/librusty_v8_release_<target>.a.gz
# Fails when Cargo.lock's v8 is not VERSION, so a bump can't leave these behind.
set -euo pipefail
VERSION=152.2.0
cd "$(dirname "$0")/.."
locked=$(awk '/^name = "v8"$/ {getline; gsub(/[^0-9.]/, "", $0); print; exit}' Cargo.lock)
if [ "$locked" != "$VERSION" ]; then
  echo "scripts/v8-archive-sha256.sh pins v8 $VERSION but Cargo.lock has ${locked:-none}: update its hashes" >&2
  exit 1
fi
case ${1:?usage: v8-archive-sha256.sh <target triple>} in
  aarch64-apple-darwin) echo a4a5484444ad4b5f10dc7bed141236dd9dbeabf2006d14520f19b935d4db4227 ;;
  aarch64-apple-ios) echo 9d2e64a2409a4b812e3d1d29da3e0a06b020c8ab1e0fed6fd9342523ba8b757c ;;
  aarch64-apple-ios-sim) echo 62f4e352b7e97baa9b59378b2eed656b9fcb1300cb6c9d79507ce85de9f5cd23 ;;
  x86_64-unknown-linux-gnu) echo b6683e9afcb77fbd8cb2c3acf45d34d52029ab216b332522c95179c12f0fed3c ;;
  *) echo "no pinned V8 archive for $1" >&2; exit 1 ;;
esac
