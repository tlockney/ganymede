#!/bin/sh
# Copies what the bench app bundles (the workloads, dist/ and the Worker
# page) into ios/Resources/bench. Run `npm ci && npm run build` in bench/
# first.
set -eu
cd "$(dirname "$0")/.."
test -f bench/dist/strip-bundle.js || { echo "run 'npm ci && npm run build' in bench/ first" >&2; exit 1; }
rm -rf ios/Resources/bench
mkdir -p ios/Resources/bench
cp -R bench/workloads bench/web bench/dist ios/Resources/bench/
echo "staged ios/Resources/bench"
