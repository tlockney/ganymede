#!/bin/sh
# Tests scripts/ci-changes.sh (#418, #457, #737, #841): for each case, a
# scratch repository with a base commit and a second commit that touches
# the given paths, then the real script on it as a pull request would see
# it (HEAD^1 is the base), and the four outputs it prints.
#
# Usage: scripts/test-ci-changes.sh
set -eu

script=$(cd "$(dirname "$0")" && pwd)/ci-changes.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
fail=0
n=0

# run_case <name> <event> <expected: code,ffi,deps,macos> <path>...
run_case() {
  name=$1 event=$2 want=$3
  shift 3
  n=$((n + 1))
  repo=$work/repo$n
  mkdir "$repo"
  (
    cd "$repo"
    git init -q .
    git config user.email t@example.invalid
    git config user.name t
    git config commit.gpgsign false
    echo base > base.txt
    git add -A
    git commit -q -m base
    for p in "$@"; do
      mkdir -p "$(dirname "$p")"
      echo "$n $p" >> "$p"
    done
    git add -A
    git commit -q -m change
  )
  got=$(cd "$repo" && GITHUB_EVENT_NAME=$event "$script" | tr '\n' ' ')
  # Four outputs, in a fixed order, written out for comparison.
  c=${want%%,*}
  rest=${want#*,}
  f=${rest%%,*}
  rest=${rest#*,}
  d=${rest%%,*}
  m=${rest#*,}
  expected="code=$c ffi=$f deps=$d macos=$m "
  if [ "$got" = "$expected" ]; then
    echo "ok   $name"
  else
    echo "FAIL $name: got '$got', want '$expected'"
    fail=1
  fi
}

PR=pull_request
#          name                              event code,ffi,deps,macos  paths
run_case "docs only"                          $PR false,false,false,false docs/guide.md README.md site/index.html examples/a.ipynb spikes/x/main.rs
run_case "docs/reference.md"                  $PR true,false,false,true docs/reference.md
run_case "docs/mcp-tools.md"                  $PR true,false,false,true docs/mcp-tools.md
run_case "v8-spike workflow"                  $PR false,false,false,false .github/workflows/v8-spike.yml
run_case "evals only"                         $PR true,false,false,false evals/journal/x.mjs
run_case "plugins only"                       $PR true,false,false,false plugins/ganymede-journal/x.json
run_case "container only"                     $PR true,false,false,false container/Dockerfile
run_case "release workflow only"              $PR true,false,false,false .github/workflows/release.yml
run_case "dependabot config"                   $PR true,false,false,false .github/dependabot.yml
run_case "script no macOS job calls"          $PR true,false,false,false scripts/release.sh
run_case "ffi scripts"                        $PR true,true,false,false scripts/ffi-smoke.sh scripts/build-xcframework.sh
run_case "v8 archive pins alone"               $PR true,true,false,false scripts/v8-archive-sha256.sh
run_case "cli crate"                          $PR true,false,false,true crates/ganymede-cli/src/x.rs
run_case "engine crate"                       $PR true,false,false,true crates/ganymede-engine/src/x.rs
run_case "other crate"                        $PR true,true,false,true crates/ganymede-core/src/x.rs
run_case "cli Cargo.toml"                     $PR true,false,true,true crates/ganymede-cli/Cargo.toml
run_case "Cargo.lock"                         $PR true,true,true,true Cargo.lock
run_case "deny.toml"                          $PR true,true,true,true deny.toml
run_case "site/install.sh"                    $PR true,false,false,true site/install.sh
run_case "validation"                         $PR true,false,false,true validation/x.spec.mjs
run_case "e2e"                                $PR true,false,false,true e2e/collab/x.py
run_case "test-workstation.sh"                $PR true,false,false,true scripts/test-workstation.sh
run_case "test-install.sh"                    $PR true,false,false,true scripts/test-install.sh
run_case "ci.yml"                             $PR true,true,false,true .github/workflows/ci.yml
run_case "unknown top-level file"             $PR true,true,false,true something.toml
run_case "evals plus a crate"                 $PR true,true,false,true evals/x.mjs crates/ganymede-core/src/x.rs
run_case "docs plus evals"                    $PR true,false,false,false docs/guide.md evals/x.mjs
run_case "evals plus site/install.sh"         $PR true,false,false,true evals/x.mjs site/install.sh
run_case "schedule runs everything"           schedule true,true,true,true docs/guide.md
run_case "manual run runs everything"         workflow_dispatch true,true,true,true docs/guide.md
run_case "no event runs everything"           "" true,true,true,true docs/guide.md

# A push to main diffs against BASE_SHA, or HEAD^1 without it (#458).
n=$((n + 1))
repo=$work/push
mkdir "$repo"
(
  cd "$repo"
  git init -q .
  git config user.email t@example.invalid
  git config user.name t
  git config commit.gpgsign false
  echo base > base.txt
  git add -A
  git commit -q -m base
  mkdir evals
  echo x > evals/x
  git add -A
  git commit -q -m change
)
got=$(cd "$repo" && GITHUB_EVENT_NAME=push "$script" | tr '\n' ' ')
if [ "$got" = "code=true ffi=false deps=false macos=true " ]; then
  echo "ok   push without BASE_SHA"
else
  echo "FAIL push without BASE_SHA: got '$got'"
  fail=1
fi
got=$(cd "$repo" && GITHUB_EVENT_NAME=push BASE_SHA=0000000000000000000000000000000000000000 "$script" | tr '\n' ' ')
if [ "$got" = "code=true ffi=true deps=true macos=true " ]; then
  echo "ok   push with a new branch's null base"
else
  echo "FAIL push with a new branch's null base: got '$got'"
  fail=1
fi

[ "$fail" = 0 ] || exit 1
echo "all passed"
