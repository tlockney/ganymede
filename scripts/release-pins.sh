#!/usr/bin/env bash
# Moves the pins on other projects' releases to their latest release, so a
# release picks them up in its own commit instead of in separate pull
# requests (#673). scripts/release.sh runs it; it can also run alone.
#
#   cdp-kernel   npm's latest @tlockney/cdp-kernel: CDP_KERNEL_VERSION in
#                ci.yml and container/Dockerfile, and the docs that name
#                the version CI tests
#   ganymede-ui  its latest GitHub release: DEFAULT_VERSION and
#                DEFAULT_SHA256 in crates/ganymede-cli/src/ui.rs, the
#                version `ganymede ui install` installs by default. The
#                SHA-256 is the release's SHA256SUMS line for the zip,
#                checked against the zip itself.
#
#   scripts/release-pins.sh           # writes the new pins, prints each one
#   scripts/release-pins.sh --check   # prints them, changes nothing
#
# Fails if a latest release is older than the pin already on main.
# GANYMEDE_NPM_URL and GANYMEDE_GITHUB_URL / GANYMEDE_GITHUB_API point it at
# other servers (tests).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

check=false
[[ "${1:-}" == --check ]] && check=true
npm=${GANYMEDE_NPM_URL:-https://registry.npmjs.org}
github=${GANYMEDE_GITHUB_URL:-https://github.com}
api=${GANYMEDE_GITHUB_API:-https://api.github.com}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Whether $1 is an older version than $2.
older() { [[ "$1" != "$2" && "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" == "$1" ]]; }

# Replaces the one line of file $1 that contains the text $2 with $3, and
# fails unless exactly one line contains it.
replace_line() {
  local file=$1 text=$2 line=$3 n
  n=$(grep -cF -- "$text" "$file" || true)
  if [[ "$n" != 1 ]]; then
    echo "error: expected one line containing '$text' in $file, found $n" >&2
    exit 1
  fi
  if ! $check; then
    TEXT=$text LINE=$line awk 'index($0, ENVIRON["TEXT"]) { print ENVIRON["LINE"]; next } { print }' "$file" > "$tmp/out"
    cat "$tmp/out" > "$file"
  fi
}

# Replaces the text $2 with $3 in the one line of file $1 that contains $2.
replace_in_line() {
  local file=$1 old=$2 new=$3 line
  line=$(grep -F -- "$old" "$file" | head -n 1)
  replace_line "$file" "$old" "${line/"$old"/"$new"}"
}

# cdp-kernel
cdp_pin=$(sed -n "s/^  CDP_KERNEL_VERSION: .* || '\([^']*\)' }}$/\1/p" .github/workflows/ci.yml)
cdp_latest=$(curl -fsSL "$npm/@tlockney%2fcdp-kernel/latest" | jq -r .version)
if [[ -z "$cdp_pin" || -z "$cdp_latest" || "$cdp_latest" == null ]]; then
  echo "error: couldn't read the cdp-kernel pin ($cdp_pin) or npm's latest ($cdp_latest)" >&2
  exit 1
fi
if older "$cdp_latest" "$cdp_pin"; then
  echo "error: npm's latest cdp-kernel $cdp_latest is older than the pin $cdp_pin" >&2
  exit 1
fi
if [[ "$cdp_latest" == "$cdp_pin" ]]; then
  echo "cdp-kernel $cdp_pin: latest"
else
  echo "cdp-kernel $cdp_pin -> $cdp_latest"
  v=$cdp_latest
  replace_line .github/workflows/ci.yml "  CDP_KERNEL_VERSION: " \
    "  CDP_KERNEL_VERSION: \${{ github.event_name == 'schedule' && 'latest' || '$v' }}"
  replace_line container/Dockerfile "ARG CDP_KERNEL_VERSION=" "ARG CDP_KERNEL_VERSION=$v"
  replace_in_line docs/architecture.md "cdp-kernel $cdp_pin in the e2e suite" "cdp-kernel $v in the e2e suite"
  replace_in_line docs/development.md "Node 24, cdp-kernel $cdp_pin," "Node 24, cdp-kernel $v,"
  replace_in_line docs/install.md "\"version\": \"$cdp_pin\"" "\"version\": \"$v\""
fi

# ganymede-ui
ui=crates/ganymede-cli/src/ui.rs
ui_pin=$(sed -n 's/^pub const DEFAULT_VERSION: &str = "\(.*\)";$/\1/p' "$ui")
ui_tag=$(curl -fsSL "$api/repos/tlockney/ganymede-ui/releases/latest" | jq -r .tag_name)
ui_latest=${ui_tag#v}
if [[ -z "$ui_pin" || -z "$ui_latest" || "$ui_tag" == null ]]; then
  echo "error: couldn't read the ganymede-ui pin ($ui_pin) or its latest release ($ui_tag)" >&2
  exit 1
fi
if older "$ui_latest" "$ui_pin"; then
  echo "error: ganymede-ui's latest release $ui_latest is older than the pin $ui_pin" >&2
  exit 1
fi
if [[ "$ui_latest" == "$ui_pin" ]]; then
  echo "ganymede-ui $ui_pin: latest"
else
  zip=ganymede-ui-$ui_latest.zip
  base="$github/tlockney/ganymede-ui/releases/download/$ui_tag"
  curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
  sum=$(awk -v f="$zip" '$2 == f || $2 == "*" f { print tolower($1) }' "$tmp/SHA256SUMS")
  curl -fsSL -o "$tmp/$zip" "$base/$zip"
  actual=$(sha256sum "$tmp/$zip" 2> /dev/null || shasum -a 256 "$tmp/$zip")
  actual=${actual%% *}
  if [[ ! "$sum" =~ ^[0-9a-f]{64}$ || "$sum" != "$actual" ]]; then
    echo "error: $zip's SHA-256 is $actual; SHA256SUMS says '${sum}'" >&2
    exit 1
  fi
  echo "ganymede-ui $ui_pin -> $ui_latest (sha256 $sum)"
  replace_line "$ui" 'pub const DEFAULT_VERSION: &str = ' "pub const DEFAULT_VERSION: &str = \"$ui_latest\";"
  replace_line "$ui" 'const DEFAULT_SHA256: &str = ' "const DEFAULT_SHA256: &str = \"$sum\";"
fi
