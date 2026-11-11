#!/usr/bin/env bash
# Prepare a release: bump the workspace version, regenerate CHANGELOG.md
# from the merged pull requests, move the cdp-kernel and ganymede-ui pins
# to their latest releases (scripts/release-pins.sh), and commit
# "Release X.Y.Z" on a release/vX.Y.Z branch. With --execute it pushes
# the branch and, with gh, opens an issue and a PR for it. Merging the PR
# tags vX.Y.Z (tag-release.yml), which publishes the binaries
# (release.yml).
#
#   scripts/release.sh minor            # dry run: shows the bump and changelog
#   scripts/release.sh minor --execute  # commits, pushes release/vX.Y.Z, opens the PR
#
# If --execute fails after committing, run it again from release/vX.Y.Z:
# it pushes the branch and opens the issue and PR it hasn't opened yet.
#
# The level is patch, minor, major, or an explicit version (0.2.0-rc.1).
# Needs cargo-release and git-cliff (cargo install --locked cargo-release
# git-cliff), curl and jq.
set -euo pipefail

if [[ $# -lt 1 || "$1" == -h || "$1" == --help ]]; then
  sed -n '2,18s/^# \{0,1\}//p' "$0"
  exit 2
fi

for tool in cargo-release git-cliff; do
  if ! command -v "$tool" > /dev/null; then
    echo "error: $tool not found; cargo install --locked cargo-release git-cliff" >&2
    exit 1
  fi
done

cd "$(git rev-parse --show-toplevel)"

execute=false
for arg in "$@"; do
  [[ "$arg" == -x || "$arg" == --execute ]] && execute=true
done

# Bumps and commits on an up-to-date main, then moves the commit to
# release/vX.Y.Z. A dry run stops after the preview.
prepare() {
  # git-cliff rebuilds CHANGELOG.md from history. A shallow clone can't
  # reach older releases' merges, and their entries would drop out
  # (ganymede-ui#179).
  if [[ "$(git rev-parse --is-shallow-repository)" == true ]]; then
    echo "Shallow clone: fetching the full history for the changelog" >&2
    git fetch --quiet --unshallow --tags origin main
  fi
  git fetch --quiet --tags origin main
  if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
    echo "error: release from an up-to-date main (HEAD is not origin/main)" >&2
    exit 1
  fi

  # The latest cdp-kernel and ganymede-ui releases go into this release
  # (scripts/release-pins.sh). Checked first, so a failure leaves main as
  # it was.
  scripts/release-pins.sh --check
  cargo release --workspace "$@"
  $execute || exit 0

  if ! pins=$(scripts/release-pins.sh); then
    git checkout --quiet -- .
    git reset --quiet --keep HEAD~1
    echo "error: couldn't update the pins; the release commit is undone" >&2
    exit 1
  fi
  echo "$pins"

  # e2e/wasm-smoke is its own workspace, so cargo-release leaves its
  # lockfile naming the old version of each crate, and CI builds it
  # --locked (#636). --workspace leaves the registry dependencies already
  # in the lockfile where they are. If the refresh fails, the commit is
  # undone, so main stays as it was and a rerun starts over.
  if ! cargo update --quiet --workspace --manifest-path e2e/wasm-smoke/Cargo.toml; then
    git checkout --quiet -- .
    git reset --quiet --keep HEAD~1
    echo "error: couldn't refresh e2e/wasm-smoke/Cargo.lock; the release commit is undone" >&2
    exit 1
  fi
  # The pins it moved go in the commit message, and from there into the
  # pull request.
  git commit --quiet --amend --all -m "$(git log -1 --format=%s)" -m "Pins:
$pins"

  version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
  branch="release/v$version"
  if git show-ref --quiet --verify "refs/heads/$branch" ||
    git ls-remote --exit-code --heads origin "$branch" > /dev/null; then
    # Undo this run's commit; main stays as it was.
    git reset --quiet --keep HEAD~1
    echo "error: $branch exists from an earlier run (here or on origin); switch to it and rerun with --execute, or delete it" >&2
    exit 1
  fi
  git switch --quiet -c "$branch"
  git branch --force main origin/main
}

# A rerun on the branch an earlier --execute made: its commit is done,
# so only the steps after it (push, issue, PR) run again.
branch=$(git branch --show-current)
if [[ "$branch" == release/v* ]]; then
  version=${branch#release/v}
  if ! $execute || [[ "$(git log -1 --format=%s)" != "Release $version" ]]; then
    echo "error: on $branch; to resume, run with --execute from its \"Release $version\" commit" >&2
    exit 1
  fi
  echo "Resuming the release of $version from $branch" >&2
else
  prepare "$@"
fi
git push --quiet -u origin "$branch"

# Docs for features not yet released say "Unreleased." (CLAUDE.md). The
# ones this release ships come out on $branch before it merges.
# In Markdown a note is what docs/scripts/check-unreleased.mjs lets
# through: a quoted line holding **Unreleased.** outside fenced blocks
# and code spans. The template in docs/development.md, and prose naming
# the words, are not (#491). The landing page has no fixed form, so any
# "Unreleased." there counts.
unreleased=$(awk '
  FNR == 1 { fenced = 0 }
  FILENAME !~ /\.md$/ { if (/Unreleased\./) print FILENAME ":" FNR ":" $0; next }
  /^[[:space:]]*(```|~~~)/ { fenced = !fenced; next }
  fenced { next }
  { text = $0; gsub(/`[^`]*`/, "", text) }
  text ~ /^[[:space:]]*>/ && index(text, "**Unreleased.**") { print FILENAME ":" FNR ":" $0 }
' README.md docs/*.md site/index.html)
if [ -n "$unreleased" ]; then
  echo "Unreleased notes; remove those this release ships on $branch:"
  echo "$unreleased"
  unreleased_body="

Before merging, remove the \"Unreleased\" notes for features this release ships:

\`\`\`
$unreleased
\`\`\`"
fi

if ! command -v gh > /dev/null; then
  echo "Pushed $branch. Open an issue \"Release v$version\" and a PR from $branch that closes it."
  exit 0
fi
if [[ "$(gh pr view "$branch" --json state --jq .state 2> /dev/null)" == OPEN ]]; then
  echo "The PR for $branch is already open: $(gh pr view "$branch" --json url --jq .url)"
  exit 0
fi
# An issue an earlier run opened before failing.
issue=$(gh issue list --state open --search "\"Release v$version\" in:title" --json number,title \
  --jq ".[] | select(.title == \"Release v$version\") | .number" | head -n 1)
if [ -z "$issue" ]; then
  issue=$(gh issue create --title "Release v$version" \
    --body "Publish v$version: binaries for all targets and the CHANGELOG.md section as notes.")
fi
gh pr create --base main --head "$branch" --title "Release v$version" \
  --body "Closes #${issue##*/}

Bumps the workspace to $version and updates CHANGELOG.md. Merging tags v$version, which publishes the release.

$(git log -1 --format=%b)${unreleased_body:-}"
