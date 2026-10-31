#!/bin/sh
# nb-cli (https://github.com/jupyter-ai-contrib/nb-cli), the notebook CLI
# built for agents, against `ganymede serve`.
#
# Remote mode: `nb connect --server --token` (its auto-detect runs
# `jupyter server list`, which needs Python), then create, cell
# add/update/delete, execute, read, search --with-errors and output clear,
# with the notebook on the server. nb-cli first tries jupyter-server-documents
# (`POST /api/fileid/index`) and jupyter-collaboration
# (`PUT /api/collaboration/session`); Ganymede serves neither, so it falls
# back to the contents API and the kernel websocket. Kernel state must
# survive separate `nb execute` calls: the journaling flow depends on it.
#
# Gateway mode: `nb execute --gateway --gateway-token --kernel-id` on a
# notebook kept on local disk, using only the kernel-gateway subset.
#
# Usage: validation/nb-cli.sh <ganymede-binary> <kernel>
#   e.g. validation/nb-cli.sh target/release/ganymede cdp-node
#        validation/nb-cli.sh target/release/ganymede python3
# Env:
#   NB              nb binary (default: `nb` on PATH); `cargo install nb-cli`
#   KERNELSPEC_ROOT passed to `serve --kernelspec-root` (a dir holding kernels/)
#   SET_CODE, READ_CODE, ERROR_CODE  cells for other kernels; the defaults
#                   cover cdp-node/cdp-deno (TypeScript) and python3
#   KEEP=1          keep the work dir
#
# `nb create` looks the kernel up on local disk, not on the server, and
# reads only VIRTUAL_ENV and the platform data dirs (not JUPYTER_PATH). So
# the script writes the server's kernelspec into a directory and points
# VIRTUAL_ENV at it, as an agent would have to.

set -eu

bin=${1:?usage: nb-cli.sh <ganymede-binary> <kernel>}
kernel=${2:?usage: nb-cli.sh <ganymede-binary> <kernel>}
NB=${NB:-nb}
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
command -v "$NB" >/dev/null || { echo "nb not found; set NB or cargo install nb-cli" >&2; exit 2; }
command -v node >/dev/null || { echo "node is needed to read JSON" >&2; exit 2; }

case "$kernel" in
python*)
	: "${SET_CODE:=x = 40}" "${READ_CODE:=x + 2}" "${ERROR_CODE:=1/0}"
	;;
*)
	: "${SET_CODE:=let x = 40}" "${READ_CODE:=x + 2}" "${ERROR_CODE:=throw new Error('boom')}"
	;;
esac

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
	[ ! -f "$work/serve.log" ] || tail -20 "$work/serve.log" >&2
	exit 1
}
step() { printf '\n== %s\n' "$*"; }
json() { node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const j=JSON.parse(s);console.log($1)})"; }

mkdir -p "$work/root" "$work/agent" "$work/gw" "$work/env/share/jupyter/kernels"
token=nbcli-$$

step "ganymede serve"
set -- serve --port 0 --token "$token" --root-dir "$work/root" -v --log-file "$work/serve.log"
[ -z "${KERNELSPEC_ROOT:-}" ] || set -- "$@" --kernelspec-root "$KERNELSPEC_ROOT"
"$bin" "$@" >"$work/serve.out" 2>"$work/serve.err" &
server=$!
i=0
while ! grep -q '^http' "$work/serve.out" 2>/dev/null; do
	i=$((i + 1))
	[ $i -lt 100 ] || fail "server did not start: $(cat "$work/serve.err")"
	sleep 0.1
done
url=$(head -1 "$work/serve.out" | sed 's#/*?token=.*##')
echo "$url"
api() { curl -sf -H "Authorization: token $token" "$@"; }

spec=$(api "$url/api/kernelspecs" | json "JSON.stringify((j.kernelspecs['$kernel']||{}).spec||null)")
[ "$spec" != null ] || fail "server has no kernelspec $kernel"
mkdir -p "$work/env/share/jupyter/kernels/$kernel"
printf '%s\n' "$spec" >"$work/env/share/jupyter/kernels/$kernel/kernel.json"
export VIRTUAL_ENV="$work/env"

kernels() { api "$url/api/kernels" | json 'j.length'; }

# --- Remote mode --------------------------------------------------------

cd "$work/agent"

step "nb connect --server --token"
"$NB" connect --server "$url" --token "$token" | tee "$work/connect.out"
grep -q 'direct (no collaboration backend)' "$work/connect.out" ||
	fail "expected direct mode (contents API + websocket)"
"$NB" status >/dev/null || fail "nb status"

step "nb create"
"$NB" create j.ipynb -k "$kernel"
[ -f "$work/root/j.ipynb" ] || fail "j.ipynb not created in the server root"
[ ! -e "$work/agent/j.ipynb" ] || fail "j.ipynb written locally in remote mode"

step "nb cell update/add/delete"
"$NB" cell update j.ipynb --cell-index 0 --source "$SET_CODE"
"$NB" cell add j.ipynb --source '# Notes' --type markdown
"$NB" cell add j.ipynb --source "$READ_CODE"
"$NB" cell add j.ipynb --source 'scratch'
"$NB" cell delete j.ipynb --cell-index -1
n=$(json 'j.cells.length' <"$work/root/j.ipynb")
[ "$n" = 3 ] || fail "expected 3 cells on disk, got $n"

step "nb execute (whole notebook)"
"$NB" execute j.ipynb >"$work/exec1.out" || fail "execute: $(cat "$work/exec1.out")"
"$NB" read j.ipynb >"$work/read1.out"
grep -q '42' "$work/read1.out" || fail "no 42 in outputs: $(cat "$work/read1.out")"
grep -q '"execution_count":2' "$work/read1.out" || fail "no execution_count 2"
json "JSON.stringify(j.cells[2].outputs)" <"$work/root/j.ipynb" | grep -q 42 ||
	fail "output not saved to the file on the server"
[ "$(kernels)" = 1 ] || fail "expected one kernel, got $(kernels)"

step "kernel state across separate nb execute calls"
"$NB" cell add j.ipynb --source "$READ_CODE"
"$NB" execute j.ipynb --cell-index -1 >"$work/exec2.out"
grep -q '42' "$work/exec2.out" || fail "state lost between calls: $(cat "$work/exec2.out")"
grep -q '"execution_count":3' "$work/exec2.out" || fail "new kernel used (execution_count reset)"
[ "$(kernels)" = 1 ] || fail "second call started another kernel"

step "error cell, nb search --with-errors"
"$NB" cell add j.ipynb --source "$ERROR_CODE"
"$NB" execute j.ipynb --cell-index -1 >"$work/exec3.out" || true
grep -q '"output_type":"error"' "$work/exec3.out" || fail "no error output: $(cat "$work/exec3.out")"
"$NB" search j.ipynb --with-errors | tee "$work/search.out" | head -1
grep -q 'Found 1 cell' "$work/search.out" || fail "search --with-errors"

step "nb output clear"
"$NB" output clear j.ipynb
! "$NB" read j.ipynb | grep -q '@@output' || fail "outputs left after clear"

step "nb execute --restart-kernel"
"$NB" execute j.ipynb --restart-kernel --cell-index 0 >/dev/null
grep -q 'uri=/api/kernels/[^ ]*/restart' "$work/serve.log" || fail "no restart request"

"$NB" disconnect >/dev/null

# --- Gateway mode -------------------------------------------------------

cd "$work/gw"

step "nb execute --gateway (local notebook, kernel on ganymede)"
"$NB" create g.ipynb -k "$kernel" >/dev/null
[ -f g.ipynb ] || fail "gateway-mode notebook not local"
"$NB" cell update g.ipynb --cell-index 0 --source "$SET_CODE" >/dev/null
"$NB" cell add g.ipynb --source "$READ_CODE" >/dev/null
# Without --kernel-id nb takes the first kernel listed, whatever its name.
kid=$(api -X POST -d "{\"name\":\"$kernel\"}" "$url/api/kernels" | json 'j.id')
gw() { "$NB" execute g.ipynb --gateway "$url" --gateway-token "$token" --kernel-id "$kid" "$@"; }
gw >"$work/gw1.out" || fail "gateway execute: $(cat "$work/gw1.out")"
json "JSON.stringify(j.cells[1].outputs)" <g.ipynb | grep -q 42 || fail "gateway output not saved locally"
gw --cell-index -1 >"$work/gw2.out"
grep -q '"execution_count":3' "$work/gw2.out" || fail "gateway kernel state not kept"

# --- What nb-cli called -------------------------------------------------

step "requests"
grep 'started processing' "$work/serve.log" |
	sed -E 's/.*method=([A-Z]+) uri=([^ ?]*).*/\1 \2/; s/[0-9a-f-]{36}/{id}/g' |
	sort | uniq -c
bad=$(grep 'finished processing' "$work/serve.log" | grep -E 'status=(4|5)[0-9][0-9]' |
	grep -vE 'uri=/api/(fileid/index|collaboration/session)' || true)
[ -z "$bad" ] || fail "unexpected errors: $bad"

printf '\nPASS: nb-cli on %s\n' "$kernel"
