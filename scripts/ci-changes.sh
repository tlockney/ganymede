#!/bin/sh
# Whether a pull request needs the build and test jobs in ci.yml (#418).
# Prints `code=true|false`, for $GITHUB_OUTPUT.
#
# On a pull request, actions/checkout checks out the merge commit, so
# HEAD^1 is the base and `git diff HEAD^1 HEAD` is what the PR changes.
# Docs, the site and Markdown can't change what the jobs test (pages.yml
# builds the site); site/install.sh can, and anything not listed counts
# as code. Any other event (a push to main, the schedule, a manual run)
# runs everything.
set -eu

if [ "${GITHUB_EVENT_NAME:-}" != pull_request ]; then
  echo code=true
  exit 0
fi

code=false
files=$(git diff --name-only HEAD^1 HEAD)
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case $f in
    site/install.sh) code=true ;;
    *.md | docs/* | site/* | LICENSE | cliff.toml) ;;
    *) code=true ;;
  esac
done <<EOF2
$files
EOF2

echo "code=$code"
