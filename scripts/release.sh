#!/usr/bin/env bash
# Prepare a release: bump the workspace version, regenerate CHANGELOG.md
# from the merged pull requests, and commit "Release X.Y.Z" on a
# release/vX.Y.Z branch. With --execute it pushes the branch and, with gh,
# opens an issue and a PR for it. Merging the PR tags vX.Y.Z
# (tag-release.yml), which publishes the binaries (release.yml).
#
#   scripts/release.sh minor            # dry run: shows the bump and changelog
#   scripts/release.sh minor --execute  # commits, pushes release/vX.Y.Z, opens the PR
#
# The level is patch, minor, major, or an explicit version (0.2.0-rc.1).
# Needs cargo-release and git-cliff: cargo install --locked cargo-release git-cliff
set -euo pipefail

if [[ $# -lt 1 || "$1" == -h || "$1" == --help ]]; then
  sed -n '2,12s/^# \{0,1\}//p' "$0"
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

execute=false
for arg in "$@"; do
  [[ "$arg" == -x || "$arg" == --execute ]] && execute=true
done

cargo release --workspace "$@"
$execute || exit 0

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
branch="release/v$version"
git switch --quiet -c "$branch"
git branch --force main origin/main
git push --quiet -u origin "$branch"

if ! command -v gh > /dev/null; then
  echo "Pushed $branch. Open an issue \"Release v$version\" and a PR from $branch that closes it."
  exit 0
fi
issue=$(gh issue create --title "Release v$version" \
  --body "Publish v$version: binaries for all targets and the CHANGELOG.md section as notes.")
gh pr create --base main --head "$branch" --title "Release v$version" \
  --body "Closes #${issue##*/}

Bumps the workspace to $version and updates CHANGELOG.md. Merging tags v$version, which publishes the release."
