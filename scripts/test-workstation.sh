#!/bin/sh
# A fresh workstation with no Python, set up the way docs/install.md and
# docs/workstation.md say: site/install.sh installs a ganymede binary
# packaged as release.yml does (served over file://), cdp-kernel comes from
# npm, `ganymede lab install` fetches JupyterLab, and `ganymede lab
# --no-browser` serves it. Playwright then logs in with the printed URL and
# runs TypeScript cells with HTML, JavaScript, and widget outputs, saves,
# and reopens them. Last, everything is uninstalled as the page says.
# docs/workstation.md uses only commands this script runs; change both
# together.
#
# Python is hidden: PATH is a clean bin dir with only the tools below
# linked in, and shims named python, python3, pip, pip3, and jupyter that
# record the call and fail. The run fails if a shim was called, or if a
# python process was seen among this script's descendants.
#
# Usage: scripts/test-workstation.sh <ganymede-binary>
# Needs: Node 24+ (and npm) on PATH, and validation/node_modules with
# Playwright's Chromium installed (cd validation && npm ci && npx
# playwright install chromium). Downloads cdp-kernel from npm and
# JupyterLab from PyPI. HOME is a new directory, so nothing outside the
# work dir changes; KEEP=1 keeps it.

set -eu

bin=${1:?usage: test-workstation.sh <ganymede-binary>}
repo=$(cd "$(dirname "$0")/.." && pwd)
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
server=
sampler=
cleanup() {
	if [ -n "${OUT_DIR:-}" ] && [ -e "$work/lab.out" ]; then
		mkdir -p "$OUT_DIR"
		cp "$work/lab.out" "$work/lab.err" "$work/serve.out" "$work/serve.err" "$OUT_DIR/" 2>/dev/null || true
	fi
	[ -z "$server" ] || kill "$server" 2>/dev/null || true
	[ -z "$sampler" ] || kill "$sampler" 2>/dev/null || true
	if [ -n "${KEEP:-}" ]; then echo "kept $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM

fail() {
	echo "FAIL: $*" >&2
	exit 1
}
step() { printf '\n== %s\n' "$*"; }

# --- A release of the binary under test, laid out as on GitHub ----------

case "$(uname -s)-$(uname -m)" in
Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
Darwin-arm64) target=aarch64-apple-darwin ;;
Darwin-x86_64) target=x86_64-apple-darwin ;;
*) fail "no release target for $(uname -sm)" ;;
esac
version=$("$bin" --version | sed 's/^ganymede //')
name=ganymede-$version-$target
mkdir -p "$work/dl/v$version" "$work/pkg/$name"
cp "$bin" "$repo/README.md" "$work/pkg/$name/"
(
	cd "$work/pkg"
	tar -czf "$work/dl/v$version/$name.tar.gz" "$name"
	cd "$work/dl/v$version"
	if command -v sha256sum >/dev/null; then
		sha256sum "$name.tar.gz" >"$name.tar.gz.sha256"
	else
		shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256"
	fi
)
cp "$repo/site/install.sh" "$work/dl/install.sh"

# --- PATH without Python ---------------------------------------------------

# The browsers Playwright installed live under the real HOME.
if [ -z "${PLAYWRIGHT_BROWSERS_PATH:-}" ]; then
	case "$(uname -s)" in
	Darwin) PLAYWRIGHT_BROWSERS_PATH=$HOME/Library/Caches/ms-playwright ;;
	*) PLAYWRIGHT_BROWSERS_PATH=$HOME/.cache/ms-playwright ;;
	esac
	export PLAYWRIGHT_BROWSERS_PATH
fi

clean=$work/clean-bin
shims=$work/shims
calls=$work/python-calls
mkdir -p "$clean" "$shims"
# Node and npm, what install.sh needs (sh, curl, tar, gzip, a SHA-256
# tool, uname, mktemp, cp, chmod, mv, mkdir, rm, sysctl on macOS), and
# the basics this script, npm, and cdp-kernel use.
for tool in node npm npx \
	sh bash curl tar gzip sha256sum shasum uname mktemp sysctl \
	cat cp chmod mv mkdir rm ln ls env dirname basename readlink realpath \
	head tail tr cut sed awk grep sort uniq wc date sleep kill ps id \
	touch true false test printf echo tee xargs find which; do
	path=$(command -v "$tool" 2>/dev/null) || continue
	case $path in /*) ln -s "$path" "$clean/$tool" ;; esac
done
for tool in python python3 pip pip3 jupyter; do
	printf '#!/bin/sh\necho "%s $*" >>"%s"\nexit 1\n' "$tool" "$calls" >"$shims/$tool"
	chmod 755 "$shims/$tool"
done

real_home=$HOME
export HOME="$work/home"
mkdir -p "$HOME"
# npm installs global packages into its own prefix; a workstation has that
# prefix's bin on PATH. Here it is a fresh one in the new HOME.
export NPM_CONFIG_PREFIX="$HOME/.npm-global"
unset XDG_DATA_HOME XDG_CONFIG_HOME XDG_RUNTIME_DIR JUPYTER_PATH JUPYTER_DATA_DIR \
	JUPYTER_CONFIG_DIR JUPYTER_RUNTIME_DIR GANYMEDE_DATA_DIR PYTHONPATH VIRTUAL_ENV CONDA_PREFIX
export PATH="$shims:$HOME/.local/bin:$NPM_CONFIG_PREFIX/bin:$clean"
hash -r 2>/dev/null || true

for tool in python python3 pip pip3 jupyter; do
	[ "$(command -v "$tool")" = "$shims/$tool" ] || fail "$tool is not the shim: $(command -v "$tool")"
done
for f in "$clean"/python* "$clean"/pip* "$clean"/jupyter*; do
	[ ! -e "$f" ] || fail "Python tool in $clean: $f"
done
echo "PATH=$PATH"
echo "HOME=$HOME (was $real_home)"

# Every half second, any process whose command name starts with "python"
# and that descends from this script is logged. The shims catch calls
# through PATH; this catches absolute paths like /usr/bin/python3.
seen=$work/python-processes
(
	while :; do
		ps -A -o pid= -o ppid= -o comm= 2>/dev/null | awk -v root=$$ '
			{ parent[$1] = $2; name[$1] = $3 }
			END {
				for (p in name) {
					n = name[p]; sub(".*/", "", n)
					if (n !~ /^python/) continue
					for (q = parent[p]; q > 1; q = parent[q]) if (q == root) { print p, name[p]; break }
				}
			}' >>"$seen"
		sleep 0.5
	done
) &
sampler=$!

# --- The install, as docs/install.md has it --------------------------------

step "install.sh (curl | sh) from file://$work/dl"
export GANYMEDE_DOWNLOAD_BASE="file://$work/dl"
curl -fsSL "file://$work/dl/install.sh" | sh -s -- --version "$version"
command -v ganymede >/dev/null || fail "ganymede is not on PATH after install.sh"
[ "$(command -v ganymede)" = "$HOME/.local/bin/ganymede" ] || fail "ganymede at $(command -v ganymede)"
ganymede --version
ganymede run -k fake 'hello' | tee "$work/fake.out"
grep -q 'HELLO' "$work/fake.out" || fail "fake kernel"

step "A kernel: Node $(node --version), cdp-kernel from npm"
npm install -g @tlockney/cdp-kernel
cdp-kernel install --runtime node
ganymede kernelspec list | tee "$work/kernelspecs"
grep -q '^ *cdp-node ' "$work/kernelspecs" || fail "cdp-node is not in ganymede kernelspec list"
ganymede kernelspec paths
ganymede info
ganymede run -k cdp-node 'const x: number = 2; x * 21' | tee "$work/run.out"
grep -q '42' "$work/run.out" || fail "cdp-node run"

step "ganymede lab install"
ganymede lab install
ganymede lab list
ganymede lab extension list

# Waits for a server started in the background as $server to print a URL
# matching $2 into file $1, and sets url to it.
wait_url() {
	url=
	tries=0
	while [ $tries -lt 120 ]; do
		tries=$((tries + 1))
		url=$(grep -Eo "$2" "$1" | head -n 1 || true)
		[ -n "$url" ] && break
		kill -0 "$server" 2>/dev/null || break
		sleep 0.5
	done
	cat "$1"
}
stop_server() {
	kill "$server" 2>/dev/null || true
	wait "$server" 2>/dev/null || true
	server=
}

step "ganymede lab --no-browser"
mkdir -p "$work/notebooks"
(cd "$work/notebooks" && exec ganymede lab --no-browser >"$work/lab.out" 2>"$work/lab.err") &
server=$!
wait_url "$work/lab.out" 'http://[^ ]+/lab\?token=[^ ]+'
[ -n "$url" ] || fail "no URL from ganymede lab: $(cat "$work/lab.err")"
base=${url%%/lab\?*}
token=${url##*token=}

step "Playwright"
out=${OUT_DIR:-$work/out}
mkdir -p "$out"
cd "$repo/validation"
node login.mjs "$url" cdp-node "$out/login.png" || fail "login with the printed URL"
# HTML whose <script> runs, and JavaScript, from TypeScript: the cell is
# trusted after save and reopen, so both run again.
# shellcheck disable=SC2016 # the backquotes and ${} are TypeScript
CODE='const probe: string = "probe";
await jupyter.display(jupyter.html`<b class="${probe}">bold</b><script>document.body.dataset.probeHtml = "ran"</script>`);
await jupyter.display({ "application/javascript": `document.body.dataset.probeJs = "ran"` }, { raw: true });' \
	node trust.mjs "$base" "$token" cdp-node "$out/trust.json" || fail "HTML and JavaScript outputs after reload"
RELOAD=1 node widgets.mjs "$base" "$token" cdp-node "$out/widgets.json" || fail "widget after reload"
cd "$work"
stop_server

step "First notebook, run with ganymede execute"
# The cell from docs/workstation.md's "First notebook", which the page has
# the reader type into JupyterLab and save as first.ipynb. Here it is
# written to first.ipynb directly and run the way the page's terminal
# section runs it. cdp-kernel downloads date-fns from the npm registry.
cat >"$work/notebooks/first.ipynb" <<'EOF'
{
 "cells": [
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "first",
   "metadata": {},
   "outputs": [],
   "source": [
    "import { format } from \"npm:date-fns@4\";\n",
    "\n",
    "const day: string = format(new Date(2026, 0, 2), \"EEEE, MMMM d\");\n",
    "await jupyter.display(jupyter.md`**${day}** is ${day.length} characters long.`);"
   ]
  }
 ],
 "metadata": {
  "kernelspec": { "name": "cdp-node", "display_name": "TypeScript (cdp-kernel, node)", "language": "typescript" }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
EOF
(cd "$work/notebooks" && ganymede execute first.ipynb --inplace)
grep -q 'Friday, January 2\*\* is 17 characters' "$work/notebooks/first.ipynb" ||
	fail "first.ipynb has no jupyter.display output from the npm: import"

step "ganymede serve, with the VS Code Jupyter extension's remote-server flow"
(cd "$work/notebooks" && exec ganymede serve >"$work/serve.out" 2>"$work/serve.err") &
server=$!
wait_url "$work/serve.out" 'http://[^ ]+/\?token=[^ ]+'
[ -n "$url" ] || fail "no URL from ganymede serve: $(cat "$work/serve.err")"
(cd "$repo/validation" && node vscode.cjs "$url" cdp-node) || fail "VS Code's flow against ganymede serve"
stop_server

step "Uninstall, as docs/workstation.md has it"
lab_version=$(ganymede lab list | awk 'NR == 1 { print $1 }')
[ -n "$lab_version" ] || fail "no version in ganymede lab list"
ganymede lab remove "$lab_version"
ganymede lab extension remove anywidget jupyterlab-widgets
ganymede kernelspec remove cdp-node
[ -z "$(ganymede lab list 2>/dev/null)" ] || fail "JupyterLab still installed"
[ -z "$(ganymede lab extension list 2>/dev/null)" ] || fail "extensions still installed"
if ganymede kernelspec list 2>/dev/null | grep -q '^ *cdp-node '; then
	fail "cdp-node still listed"
fi
npm uninstall -g @tlockney/cdp-kernel
rm ~/.local/bin/ganymede
hash -r 2>/dev/null || true
for tool in cdp-kernel ganymede; do
	if command -v "$tool" >/dev/null; then fail "$tool is still on PATH"; fi
done

kill "$sampler" 2>/dev/null || true
sampler=

step "No Python"
if [ -s "$calls" ]; then
	cat "$calls" >&2
	fail "Python was called through PATH"
fi
if [ -s "$seen" ]; then
	sort -u "$seen" >&2
	fail "a python process ran"
fi
echo "no python, python3, pip, or jupyter ran"
echo "workstation install: all checks passed"
