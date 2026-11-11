#!/bin/sh
# Re-runs the journals: downloads each one's data, then runs the
# notebook top to bottom and saves its outputs in place. Needs ganymede
# and cdp-kernel's cdp-node kernel (docs/install.md), Node, curl and
# network access; about 60 MB of data.
#
#   examples/journals/run.sh            # both
#   examples/journals/run.sh oklahoma   # one
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -gt 0 ] || set -- fremont oklahoma
for name in "$@"; do
  dir="$here/$name"
  "$dir/fetch-data.sh"
  for nb in "$dir"/journal/*.ipynb; do
    (cd "$dir" && ganymede execute --inplace "journal/$(basename "$nb")")
  done
done
