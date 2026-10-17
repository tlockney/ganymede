# Install

Ganymede is one binary, `ganymede`. It needs no Python, and no Jupyter
install: kernels come from kernelspecs, which each kernel's own installer
writes.

## From source

With a Rust toolchain ([rustup](https://rustup.rs)):

```sh
cargo install --git https://github.com/tlockney/ganymede ganymede-cli
ganymede --version
```

From a checkout, `cargo install --path crates/ganymede-cli` does the same.

## From a release

Releases attach a prebuilt archive per platform: Linux (x86_64,
aarch64), macOS (aarch64, x86_64), and Windows (x86_64), each with a
`.sha256` beside it and `SHA256SUMS` for all of them. Get one from the
[releases page](https://github.com/tlockney/ganymede/releases). On Linux or
macOS, this downloads the latest release, checks its SHA-256, and installs
`ganymede` into `~/.local/bin`:

```sh
VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
  https://github.com/tlockney/ganymede/releases/latest | sed 's|.*/v||')
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)                TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64)                TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64)               TARGET=x86_64-apple-darwin ;;
  *) echo "no prebuilt binary for $(uname -sm)" >&2 ;;
esac
NAME=ganymede-$VERSION-$TARGET
cd "$(mktemp -d)"
curl -fsSLO "https://github.com/tlockney/ganymede/releases/download/v$VERSION/$NAME.tar.gz"
curl -fsSLO "https://github.com/tlockney/ganymede/releases/download/v$VERSION/$NAME.tar.gz.sha256"
sha256sum -c "$NAME.tar.gz.sha256" 2>/dev/null || shasum -a 256 -c "$NAME.tar.gz.sha256"
tar -xzf "$NAME.tar.gz"
mkdir -p ~/.local/bin && install -m 755 "$NAME/ganymede" ~/.local/bin/
export PATH="$HOME/.local/bin:$PATH"   # if it is not on PATH already
```

Set `VERSION=0.1.0` instead of the first line to pin a release. The Linux
binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36, or later).
macOS does not quarantine files fetched with `curl`; after a browser
download, run `xattr -d com.apple.quarantine ganymede` before the first
run, since the binaries are not notarized.

To check the install without a kernel, use the built-in `fake` kernel:

```sh
ganymede run -k fake 'hello'   # prints "echo: hello" and "HELLO"
```

## A kernel

Ganymede runs whatever kernelspecs are installed in the Jupyter data
directories (`~/.local/share/jupyter/kernels`, `~/Library/Jupyter/kernels`
on macOS, `JUPYTER_PATH`, and the rest of Jupyter's list). For TypeScript
notebooks, install [cdp-kernel](https://cdp-kernel.org/docs/install) on
Node 24 or later:

```sh
npm install -g @tlockney/cdp-kernel
cdp-kernel install --runtime node
ganymede kernelspec list
```

Any other Jupyter kernel works the same way, ipykernel included, once its
kernelspec is installed.
