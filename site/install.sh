#!/bin/sh
# Install Ganymede from a GitHub release on Linux or macOS, then, when
# Node 24+ is present, cdp-kernel for TypeScript notebooks.
#
#   curl -fsSL https://ganymede-server.com/install.sh | sh
#   curl -fsSL https://ganymede-server.com/install.sh | sh -s -- --version 0.1.0 --no-kernel
#
# The steps are the ones on https://ganymede-server.com/docs/install.
# Everything runs inside main(), so a download cut short runs nothing.

set -eu

REPO=https://github.com/tlockney/ganymede
# Where releases are downloaded from; the CI test points it at a local
# directory (file://...).
BASE=${GANYMEDE_DOWNLOAD_BASE:-$REPO/releases/download}

say() { printf '%s\n' "$*"; }
err() {
	printf 'install.sh: %s\n' "$*" >&2
	exit 1
}
has() { command -v "$1" >/dev/null 2>&1; }

usage() {
	cat <<EOF
Install Ganymede from a GitHub release.

Usage: install.sh [options]

  --version X.Y.Z   Install this release instead of the latest.
  --bin-dir DIR     Where to put the binary (default: ~/.local/bin).
  --no-kernel       Skip installing cdp-kernel.
  --runtime NAME    cdp-kernel target runtime: node (default), deno, bun,
                    workerd, celld.
  -h, --help        Show this help.
EOF
}

fetch() { # url dest
	if has curl; then
		curl -fsSL -o "$2" "$1"
	elif has wget; then
		wget -qO "$2" "$1"
	else
		err "needs curl or wget"
	fi
}

latest_version() {
	# /releases/latest redirects to /releases/tag/vX.Y.Z; with no release
	# yet it redirects to /releases.
	if has curl; then
		url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$REPO/releases/latest")
	else
		url=$(wget -S --spider "$REPO/releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tail -n 1)
	fi
	case $url in
	*/tag/v*) printf '%s\n' "${url##*/tag/v}" | tr -d '\r' ;;
	*) err "no Ganymede release is published yet. Install from source instead:
  cargo install --git $REPO ganymede-cli" ;;
	esac
}

target() {
	case "$(uname -s)-$(uname -m)" in
	Linux-x86_64 | Linux-amd64) echo x86_64-unknown-linux-gnu ;;
	Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-gnu ;;
	Darwin-arm64 | Darwin-aarch64) echo aarch64-apple-darwin ;;
	Darwin-x86_64)
		# Rosetta reports x86_64 under a translated shell; the native
		# binary is the better pick.
		if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
			echo aarch64-apple-darwin
		else
			echo x86_64-apple-darwin
		fi
		;;
	*) err "no prebuilt binary for $(uname -sm). Install from source:
  cargo install --git $REPO ganymede-cli" ;;
	esac
}

verify() { # file.sha256, run in its directory
	if has sha256sum; then
		sha256sum -c "$1" >/dev/null
	elif has shasum; then
		shasum -a 256 -c "$1" >/dev/null
	else
		err "needs sha256sum or shasum to check the download"
	fi
}

install_kernel() { # runtime
	if ! has node || ! has npm; then
		say ""
		say "Node.js not found, so cdp-kernel was not installed. For TypeScript"
		say "notebooks, install Node 24 or later, then run:"
		say "  npm install -g @tlockney/cdp-kernel && cdp-kernel install --runtime $1"
		return 0
	fi
	major=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo 0)
	if [ "$major" -lt 24 ]; then
		say ""
		say "cdp-kernel needs Node 24 or later; found $(node --version). After"
		say "upgrading Node, run:"
		say "  npm install -g @tlockney/cdp-kernel && cdp-kernel install --runtime $1"
		return 0
	fi
	say "Installing cdp-kernel with npm"
	if ! npm install -g @tlockney/cdp-kernel >/dev/null; then
		say ""
		say "npm install -g @tlockney/cdp-kernel failed (see above). If npm's global"
		say "directory needs root, fix npm's prefix or rerun that command with sudo,"
		say "then: cdp-kernel install --runtime $1"
		return 0
	fi
	cdp-kernel install --runtime "$1"
}

main() {
	version=
	bin_dir=$HOME/.local/bin
	kernel=1
	runtime=node
	while [ $# -gt 0 ]; do
		case $1 in
		--version) version=${2:?--version needs a value}; shift 2 ;;
		--version=*) version=${1#*=}; shift ;;
		--bin-dir) bin_dir=${2:?--bin-dir needs a value}; shift 2 ;;
		--bin-dir=*) bin_dir=${1#*=}; shift ;;
		--no-kernel) kernel=; shift ;;
		--runtime) runtime=${2:?--runtime needs a value}; shift 2 ;;
		--runtime=*) runtime=${1#*=}; shift ;;
		-h | --help) usage; exit 0 ;;
		*) usage >&2; err "unknown option: $1" ;;
		esac
	done
	version=${version#v}

	has tar || err "needs tar"
	tgt=$(target)
	[ -n "$version" ] || version=$(latest_version)
	name=ganymede-$version-$tgt

	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT
	trap 'exit 1' INT TERM
	say "Downloading Ganymede $version for $tgt"
	fetch "$BASE/v$version/$name.tar.gz" "$tmp/$name.tar.gz" ||
		err "download failed: $BASE/v$version/$name.tar.gz"
	fetch "$BASE/v$version/$name.tar.gz.sha256" "$tmp/$name.tar.gz.sha256" ||
		err "download failed: $BASE/v$version/$name.tar.gz.sha256"
	(cd "$tmp" && verify "$name.tar.gz.sha256") || err "checksum mismatch for $name.tar.gz"
	tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

	mkdir -p "$bin_dir"
	# Copy then rename, so a running ganymede is replaced, not overwritten.
	cp "$tmp/$name/ganymede" "$bin_dir/.ganymede.new"
	chmod 755 "$bin_dir/.ganymede.new"
	mv -f "$bin_dir/.ganymede.new" "$bin_dir/ganymede"
	say "Installed $("$bin_dir/ganymede" --version) to $bin_dir/ganymede"

	if [ -n "$kernel" ]; then
		install_kernel "$runtime"
	fi

	case ":$PATH:" in
	*":$bin_dir:"*) ;;
	*)
		say ""
		say "$bin_dir is not on your PATH. Add it, e.g. in ~/.profile or ~/.zshrc:"
		say "  export PATH=\"$bin_dir:\$PATH\""
		;;
	esac
	say ""
	say "Next: ganymede kernelspec list"
}

main "$@"
