#!/bin/sh
# Whether a pull request needs the build and test jobs in ci.yml (#418).
# Prints `code=true|false`, `ffi=true|false`, `deps=true|false` and
# `macos=true|false`, for $GITHUB_OUTPUT.
#
# On a pull request, actions/checkout checks out the merge commit, so
# HEAD^1 is the base and `git diff HEAD^1 HEAD` is what the PR changes.
# On a push to main the base is the commit main was at before the push
# (BASE_SHA, github.event.before), or HEAD^1 without it (#458), so a
# docs-only merge doesn't run everything either. Docs, the site,
# Markdown and examples/ can't change what the jobs test (pages.yml
# builds the site), except docs/reference.md and docs/mcp-tools.md,
# which ganymede-cli's and ganymede-server's tests read, and neither
# can spikes/, which no crate depends on;
# site/install.sh can, and anything not listed counts as code. The
# schedule and manual runs run everything, and so does a push whose
# base can't be found or diffed.
#
# ffi is whether the change can affect ganymede-ffi's build, which the
# macOS ffi cell (the XCFramework) needs (#457): false when only code
# outside its dependency tree changed. ganymede-cli and ganymede-engine
# aren't in that tree; ci.yml's ffi job fails if either joins it.
#
# deps is whether the change can change the Rust dependency graph
# (Cargo.lock, a Cargo.toml, deny.toml), which is when a pull request
# checks RustSec advisories (#737): an advisory published since main
# last passed then fails the PR that changed dependencies, not an
# unrelated one. Every other event checks them regardless.
#
# macos is whether the change can affect what the macOS cells of the e2e
# and workstation jobs test (#841): false when every changed file is
# either not code (the paths above) or code whose behaviour can't differ
# on macOS because neither job builds, runs or reads it. They build and
# test the workspace (crates/, Cargo files, the toolchain), read e2e/,
# validation/, docs/reference.md and docs/mcp-tools.md, and run
# site/install.sh through scripts/test-install.sh, scripts/test-workstation.sh
# and validation/nb-cli.sh, and ci.yml defines them. evals/, plugins/,
# container/, the other workflows and scripts, and the rest of .github/
# and plugins/ are only used by Linux jobs (plugins/ganymede-journal/skills
# is compiled into ganymede-server, so it counts as code everywhere). Only pull requests filter on it: a push
# to main gives true whenever it gives code=true, so main still runs the
# macOS cells for every code change. Like the others it fails open:
# unknown paths, non-PR events and a failed diff give true.
set -eu

everything() {
  echo code=true
  echo ffi=true
  echo deps=true
  echo macos=true
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
ffi=false
deps=false
macos=false
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case $f in
    Cargo.lock | Cargo.toml | */Cargo.toml | deny.toml) deps=true ;;
  esac
  case $f in
    site/install.sh) code=true macos=true ;;
    # Its command flags are checked against the CLI (ganymede-cli's
    # reference.rs).
    docs/reference.md) code=true macos=true ;;
    # Generated from the MCP tools and checked by ganymede-server's
    # tests (src/mcp/reference.rs).
    docs/mcp-tools.md) code=true macos=true ;;
    # Compiled into ganymede-server (and so ganymede-ffi) as the skills
    # it serves over MCP, so before the `*.md` arm.
    plugins/ganymede-journal/skills/*) code=true ffi=true macos=true ;;
    *.md | docs/* | site/* | LICENSE | cliff.toml) ;;
    # Example notebooks and their data scripts, which the site renders.
    examples/*) ;;
    # Spikes with their own workspace and workflow (v8-spike.yml, #646).
    spikes/* | .github/workflows/v8-spike.yml) ;;
    crates/ganymede-cli/* | crates/ganymede-engine/*) code=true macos=true ;;
    # e2e/ is read by ganymede-cli's tests and validation/ is the
    # workstation job's Playwright and nb-cli tests.
    e2e/* | validation/*) code=true macos=true ;;
    # Run by Linux jobs only (the plugin and image jobs).
    evals/* | plugins/* | container/*) code=true ;;
    scripts/build-xcframework.sh | scripts/ffi-smoke.sh) code=true ffi=true ;;
    .github/workflows/ci.yml) code=true ffi=true macos=true ;;
    # What the macOS e2e and workstation cells run.
    scripts/test-workstation.sh | scripts/test-install.sh) code=true macos=true ;;
    scripts/* | .github/*) code=true ;;
    *) code=true ffi=true macos=true ;;
  esac
done <<EOF2
$files
EOF2

# Only pull requests skip the macOS cells; main runs them on every
# code change (#841).
[ "${GITHUB_EVENT_NAME:-}" = pull_request ] || macos=true

echo "code=$code"
echo "ffi=$ffi"
echo "deps=$deps"
echo "macos=$macos"
