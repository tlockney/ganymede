#!/bin/sh
# Whether a pull request needs the build and test jobs in ci.yml (#418).
# Prints `code=true|false`, for $GITHUB_OUTPUT.
#
# On a pull request, actions/checkout checks out the merge commit, so
# HEAD^1 is the base and `git diff HEAD^1 HEAD` is what the PR changes.
# On a push to main the base is the commit main was at before the push
# (BASE_SHA, github.event.before), or HEAD^1 without it (#458), so a
# docs-only merge doesn't run everything either. Docs, the site and
# Markdown can't change what the jobs test (pages.yml builds the site);
# site/install.sh can, and anything not listed counts as code. The
# schedule and manual runs run everything, and so does a push whose
# base can't be found or diffed.
set -eu

everything() {
  echo code=true
  exit 0
}

case ${GITHUB_EVENT_NAME:-} in
  pull_request) base=HEAD^1 ;;
  push)
    base=${BASE_SHA:-HEAD^1}
    case $base in *[!0]*) ;; *) everything ;; esac
    if ! git cat-file -e "$base^{commit}" 2> /dev/null; then
      git fetch -q --no-tags --depth=1 origin "$base" 2> /dev/null || everything
    fi
    ;;
  *) everything ;;
esac

files=$(git diff --no-renames --name-only "$base" HEAD) || everything
code=false
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
