#!/bin/sh
# Whether PyPI has newer releases than the ones `ganymede lab install`
# pins: JupyterLab (DEFAULT_VERSION in crates/ganymede-cli/src/lab.rs) and
# the default extensions (DEFAULTS in crates/ganymede-cli/src/lab/
# extension.rs). Prints each pin and fails when one is behind, so CI's
# weekly run shows it (#433). Bumping a pin means its version and the
# wheel's SHA-256 from PyPI.
set -eu
cd "$(dirname "$0")/.."
pypi=${GANYMEDE_PYPI_URL:-https://pypi.org/pypi}
behind=0

check() {
  latest=$(curl -fsS "$pypi/$1/json" | jq -r .info.version)
  if [ "$latest" = "$2" ]; then
    echo "$1 $2: latest"
  else
    echo "::warning::$1 is pinned at $2; PyPI's latest is $latest"
    behind=1
  fi
}

lab=$(sed -n 's/^pub const DEFAULT_VERSION: &str = "\(.*\)";$/\1/p' crates/ganymede-cli/src/lab.rs)
check jupyterlab "$lab"
# Each Pin's name line is followed by its version line.
sed -n '/^pub const DEFAULTS: &\[Pin\]/,/^];/p' crates/ganymede-cli/src/lab/extension.rs |
  sed -n 's/^ *\(name\|version\): "\(.*\)",$/\2/p' | paste - - > "${TMPDIR:-/tmp}/pins.$$"
while read -r name version; do
  check "$name" "$version"
done < "${TMPDIR:-/tmp}/pins.$$"
rm -f "${TMPDIR:-/tmp}/pins.$$"
[ -n "$lab" ] || { echo "::error::no DEFAULT_VERSION found in lab.rs"; exit 1; }
exit "$behind"
