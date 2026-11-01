#!/bin/sh
# jupyterlab-lsp against `ganymede serve`, with jupyter-lsp on
# `jupyter lab` as the oracle: validation/lsp.mjs must see the same
# diagnostics from typescript-language-server (a .ts file) and pyright (a
# Python notebook, through the virtual documents) on both. Both servers
# get the same two language servers from config, with jupyter-lsp's
# autodetection off, and serve the same JupyterLab and jupyterlab-lsp
# (Ganymede through --static-dir and --labextensions-path).
#
# Usage: scripts/test-lsp.sh <ganymede-binary>
# Needs on PATH: python with jupyterlab, jupyterlab-lsp, jupyter-lsp and
# ipykernel; typescript-language-server, typescript@5 and pyright-langserver;
# node, and validation/node_modules with Playwright's Chromium.
# Writes results to OUT_DIR (default: the work dir); KEEP=1 keeps it.

set -eu

bin=${1:?usage: test-lsp.sh <ganymede-binary>}
repo=$(cd "$(dirname "$0")/.." && pwd)
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
server=
cleanup() {
	[ -z "$server" ] || kill "$server" 2>/dev/null || true
	if [ -n "${KEEP:-}" ]; then echo "kept $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM

fail() {
	echo "FAIL: $*" >&2
	exit 1
}
step() { printf '\n== %s\n' "$*"; }
stop_server() {
	kill "$server" 2>/dev/null || true
	wait "$server" 2>/dev/null || true
	server=
}
# Waits for $1 to answer HTTP, or fails with the log in $2.
wait_http() {
	i=0
	until curl -sf -o /dev/null "$1"; do
		i=$((i + 1))
		[ $i -lt 120 ] || fail "no server at $1: $(tail -20 "$2")"
		kill -0 "$server" 2>/dev/null || fail "server exited: $(tail -20 "$2")"
		sleep 0.5
	done
}

for tool in typescript-language-server pyright-langserver jupyter python node; do
	command -v "$tool" >/dev/null || fail "$tool is not on PATH"
done
prefix=$(python -c 'import sys; print(sys.prefix)')
[ -d "$prefix/share/jupyter/labextensions/@jupyter-lsp/jupyterlab-lsp" ] ||
	fail "jupyterlab-lsp is not installed in $prefix"
out=${OUT_DIR:-$work/out}
mkdir -p "$out" "$work/ganymede-root" "$work/jupyter-root" "$work/jupyter-config"
token=lsp-$$

# The same specs on both sides; jupyter-lsp's version 2 spec format.
ts_mime='"text/x.typescript", "application/typescript", "text/javascript", "text/typescript"'
cat >"$work/ganymede.toml" <<EOF
[language_servers.typescript-language-server]
argv = ["typescript-language-server", "--stdio"]
languages = ["typescript", "javascript"]
mime_types = [$ts_mime]
display_name = "TypeScript"

[language_servers.pyright]
argv = ["pyright-langserver", "--stdio"]
languages = ["python"]
mime_types = ["text/x-python", "text/x-ipython"]
display_name = "Pyright"
EOF
cat >"$work/jupyter-config/jupyter_server_config.json" <<EOF
{"LanguageServerManager": {"autodetect": false, "language_servers": {
  "typescript-language-server": {"version": 2, "argv": ["typescript-language-server", "--stdio"],
    "languages": ["typescript", "javascript"], "mime_types": [$ts_mime], "display_name": "TypeScript"},
  "pyright": {"version": 2, "argv": ["pyright-langserver", "--stdio"],
    "languages": ["python"], "mime_types": ["text/x-python", "text/x-ipython"], "display_name": "Pyright"}}}}
EOF

step "jupyter lab with jupyter-lsp (oracle)"
port=$(node -e 'const s=require("net").createServer().listen(0,()=>{console.log(s.address().port);s.close()})')
(cd "$work/jupyter-root" && JUPYTER_CONFIG_DIR="$work/jupyter-config" exec jupyter lab --no-browser \
	--allow-root --port "$port" --ServerApp.port_retries=0 --IdentityProvider.token="$token" \
	>"$work/jupyter.log" 2>&1) &
server=$!
wait_http "http://127.0.0.1:$port/api" "$work/jupyter.log"
(cd "$repo/validation" && node lsp.mjs "http://127.0.0.1:$port" "$token" "$out/lsp-jupyter.json") ||
	fail "jupyterlab-lsp under jupyter-lsp (the oracle): $(cat "$out/lsp-jupyter.json")"
stop_server

step "ganymede serve"
"$bin" serve --port 0 --token "$token" --config "$work/ganymede.toml" \
	--root-dir "$work/ganymede-root" --config-dir "$work/ganymede-state" \
	--static-dir "$prefix/share/jupyter/lab/static" \
	--labextensions-path "$prefix/share/jupyter/labextensions" \
	>"$work/ganymede.out" 2>"$work/ganymede.err" &
server=$!
i=0
until url=$(grep -Eo 'http://[^ ]+' "$work/ganymede.out" | head -n 1) && [ -n "$url" ]; do
	i=$((i + 1))
	[ $i -lt 120 ] || fail "no URL from ganymede serve: $(cat "$work/ganymede.err")"
	sleep 0.5
done
base=${url%%/\?*}
base=${base%/}
(cd "$repo/validation" && node lsp.mjs "$base" "$token" "$out/lsp-ganymede.json") ||
	fail "jupyterlab-lsp against ganymede serve: $(cat "$out/lsp-ganymede.json")"
stop_server

printf '\nPASS: jupyterlab-lsp diagnostics match on Ganymede and jupyter-lsp\n'
