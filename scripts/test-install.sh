#!/bin/sh
# Tests site/install.sh against a fake release on disk: a stub `ganymede`
# packaged the way release.yml packages the real one, served over file://.
# Usage: scripts/test-install.sh [shell]   (default sh; CI also runs bash and dash)
# With TEST_KERNEL=1 it also installs cdp-kernel through the script, which
# needs Node 24+ and npm (CI only; it changes the global npm packages).
# CDP_KERNEL_VERSION pins the cdp-kernel it installs, as CI does.

set -eu

sh=${1:-sh}
script=$(cd "$(dirname "$0")/.." && pwd)/site/install.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case "$(uname -s)-$(uname -m)" in
Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
Darwin-arm64) target=aarch64-apple-darwin ;;
Darwin-x86_64)
	if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
		target=aarch64-apple-darwin
	else
		target=x86_64-apple-darwin
	fi
	;;
esac

version=9.9.9
name=ganymede-$version-$target
mkdir -p "$work/dl/v$version" "$work/$name"
printf '#!/bin/sh\necho "ganymede %s"\n' "$version" >"$work/$name/ganymede"
chmod 755 "$work/$name/ganymede"
(
	cd "$work"
	tar -czf "dl/v$version/$name.tar.gz" "$name"
	cd "dl/v$version"
	if command -v sha256sum >/dev/null; then
		sha256sum "$name.tar.gz" >"$name.tar.gz.sha256"
	else
		shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256"
	fi
)
export GANYMEDE_DOWNLOAD_BASE="file://$work/dl"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# Installs into a bin dir that isn't on PATH, and says so.
out=$("$sh" "$script" --version "v$version" --bin-dir "$work/bin" --no-kernel)
echo "$out"
[ "$("$work/bin/ganymede" --version)" = "ganymede $version" ] || fail "installed binary"
echo "$out" | grep -q "is not on your PATH" || fail "PATH hint"

# Piped into sh, as the docs show, with --version=.
"$sh" -s -- --version="$version" --bin-dir="$work/bin2" --no-kernel <"$script" >/dev/null ||
	fail "piped install"
[ -x "$work/bin2/ganymede" ] || fail "piped install binary"

# Without Node 24 the kernel step is skipped with instructions, not an error.
PATH=/nonexistent:/usr/bin:/bin "$sh" "$script" --version "$version" --bin-dir "$work/bin5" >"$work/out" 2>&1 ||
	fail "kernel step without node: $(cat "$work/out")"
grep -q "npm install -g @tlockney/cdp-kernel" "$work/out" || fail "kernel hint: $(cat "$work/out")"

if [ "${TEST_KERNEL:-}" = 1 ]; then
	kernel_path=$PATH
	if [ -n "${CDP_KERNEL_VERSION:-}" ]; then
		# install.sh installs the latest cdp-kernel, as a user gets it. An
		# npm in front of the real one pins it, so a cdp-kernel release
		# can't fail this test with no change here.
		mkdir "$work/npm-pin"
		cat >"$work/npm-pin/npm" <<-EOF
			#!/bin/sh
			for a do
				shift
				[ "\$a" = @tlockney/cdp-kernel ] && a=@tlockney/cdp-kernel@$CDP_KERNEL_VERSION
				set -- "\$@" "\$a"
			done
			exec "$(command -v npm)" "\$@"
		EOF
		chmod 755 "$work/npm-pin/npm"
		kernel_path=$work/npm-pin:$PATH
	fi
	PATH=$kernel_path JUPYTER_DATA_DIR=$work/jupyter "$sh" "$script" --version "$version" --bin-dir "$work/bin6" ||
		fail "install with cdp-kernel"
	[ -f "$work/jupyter/kernels/cdp-node/kernel.json" ] || fail "no cdp-node kernelspec"
fi

# A corrupted archive is refused and nothing is installed.
printf 'x' >>"$work/dl/v$version/$name.tar.gz"
if "$sh" "$script" --version "$version" --bin-dir "$work/bin3" --no-kernel 2>"$work/err"; then
	fail "accepted a bad checksum"
fi
grep -q "checksum mismatch" "$work/err" || fail "checksum message: $(cat "$work/err")"
[ ! -e "$work/bin3/ganymede" ] || fail "installed despite a bad checksum"

# A missing release fails with the URL.
if "$sh" "$script" --version 0.0.0 --bin-dir "$work/bin4" --no-kernel 2>"$work/err"; then
	fail "installed a missing release"
fi
grep -q "download failed" "$work/err" || fail "missing-release message: $(cat "$work/err")"

# Unknown options are rejected.
if "$sh" "$script" --bogus >/dev/null 2>&1; then fail "accepted --bogus"; fi

echo "install.sh: all tests passed with $sh"
