#!/bin/sh
# Copies ganymede-ui's docs (its docs/*.md, all but the dev log, and the
# images in docs/images/) into
# docs/ganymede-ui/, which VitePress renders under /docs/ganymede-ui/. The
# copy is not committed; ganymede-ui's main is the source.
#
#   npm run fetch-ui                         # from GitHub, main
#   GANYMEDE_UI_DIR=~/src/ganymede-ui npm run fetch-ui   # a local checkout
set -eu

docs=$(cd "$(dirname "$0")/.." && pwd)
out="$docs/ganymede-ui"
src=${GANYMEDE_UI_DIR:-}
tmp=

if [ -z "$src" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  repo=${GANYMEDE_UI_REPO:-https://github.com/tlockney/ganymede-ui.git}
  git clone --quiet --depth 1 --filter=blob:none --sparse "$repo" "$tmp/ui"
  git -C "$tmp/ui" sparse-checkout set --no-cone '/docs/*.md' '/docs/images/'
  src="$tmp/ui"
  echo "ganymede-ui $(git -C "$src" rev-parse --short HEAD)"
fi

rm -rf "$out"
mkdir -p "$out"
for f in "$src"/docs/*.md; do
  [ "$(basename "$f")" = notes.md ] && continue
  cp "$f" "$out/"
  echo "  $(basename "$f")"
done
if [ -d "$src/docs/images" ]; then
  cp -R "$src/docs/images" "$out/images"
  echo "  images/"
fi
