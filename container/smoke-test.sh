#!/bin/sh
# Tests an image built from container/Dockerfile (#203): no Python in it,
# a TypeScript cell on cdp-node, and the default command serving
# JupyterLab and the REST API behind its token. Needs curl on the host.
# Usage: container/smoke-test.sh [image]   (default ganymede)

set -eu

image=${1:-ganymede}
name=ganymede-smoke-$$
port=${SMOKE_PORT:-18888}
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT

fail() {
	printf 'FAIL %s\n' "$*" >&2
	exit 1
}

if docker run --rm "$image" sh -c 'command -v python3 || command -v python'; then
	fail "the image has Python"
fi
echo "ok   no Python"

out=$(docker run --rm "$image" ganymede run -k cdp-node 'const x: number = 2; x * 21')
[ "$out" = 42 ] || fail "cdp-node cell printed: $out"
echo "ok   cdp-node cell"

docker run -d --name "$name" -p "127.0.0.1:$port:8888" "$image" >/dev/null
url=
for _ in $(seq 60); do
	url=$(docker logs "$name" 2>/dev/null | grep -o 'http://[^ ]*/lab?token=[0-9a-f]*' | head -n 1) || true
	[ -n "$url" ] && break
	sleep 1
done
[ -n "$url" ] || {
	docker logs "$name" >&2
	fail "no URL printed"
}
token=${url##*token=}
base=http://127.0.0.1:$port

code=$(curl -s -o /dev/null -w '%{http_code}' "$base/api/status")
[ "$code" = 403 ] || fail "/api/status without the token returned $code"
curl -fsS -H "Authorization: token $token" "$base/api/kernelspecs" | grep -q '"cdp-node"' ||
	fail "/api/kernelspecs has no cdp-node"
curl -fsS -o /dev/null "$base/lab?token=$token" || fail "/lab returned an error"
echo "ok   ganymede lab serves JupyterLab and the API"
