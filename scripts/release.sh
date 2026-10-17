#!/usr/bin/env bash
# Cut a release: bump the workspace version, regenerate CHANGELOG.md from
# the merged pull requests, commit "Release X.Y.Z", tag vX.Y.Z, and push.
# The tag starts .github/workflows/release.yml, which publishes binaries.
#
#   scripts/release.sh minor            # dry run: shows what would happen
#   scripts/release.sh minor --execute  # does it
#
# The level is patch, minor, major, or an explicit version (0.2.0-rc.1).
# Needs cargo-release and git-cliff: cargo install --locked cargo-release git-cliff
set -euo pipefail

if [[ $# -lt 1 || "$1" == -h || "$1" == --help ]]; then
  sed -n '2,11s/^# \{0,1\}//p' "$0"
  exit 2
fi

for tool in cargo-release git-cliff; do
  if ! command -v "$tool" > /dev/null; then
    echo "error: $tool not found; cargo install --locked cargo-release git-cliff" >&2
    exit 1
  fi
done

cd "$(git rev-parse --show-toplevel)"
git fetch --quiet --tags origin main
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
  echo "error: release from an up-to-date main (HEAD is not origin/main)" >&2
  exit 1
fi

exec cargo release --workspace "$@"
