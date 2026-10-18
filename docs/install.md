# Install

Ganymede is one binary, `ganymede`. It needs no Python, and no Jupyter
install: kernels come from kernelspecs, which each kernel's own installer
writes.

For a first install, [Install on a workstation](workstation.md) has the
whole path from a clean Linux or macOS machine to JupyterLab, as numbered
steps. This page is the reference for each piece.

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
`.sha256` beside it, `SHA256SUMS` for all of them, and a build
provenance attestation ([Verify a download](#verify-a-download)). Get one from the
[releases page](https://github.com/tlockney/ganymede/releases).

### Install script

On Linux or macOS:

```sh
curl -fsSL https://ganymede-server.com/install.sh | sh
```

It downloads the latest release for your platform, checks its SHA-256,
and installs `ganymede` into `~/.local/bin`. If Node 24 or later is
installed, it then installs cdp-kernel and its `cdp-node` kernelspec, as
in [A kernel](#a-kernel); otherwise it prints the commands to run once
Node is there. Options go after `sh -s --`:

| Option | Effect |
| --- | --- |
| `--version X.Y.Z` | Install that release instead of the latest. |
| `--bin-dir DIR` | Install into `DIR` instead of `~/.local/bin`. |
| `--no-kernel` | Skip cdp-kernel. |
| `--runtime NAME` | cdp-kernel's target runtime: `node` (default), `deno`, `bun`, `workerd`, or `celld`. |

```sh
curl -fsSL https://ganymede-server.com/install.sh | sh -s -- --version 0.1.0 --no-kernel
```

Running it again upgrades in place. The script is
[`site/install.sh`](https://github.com/tlockney/ganymede/blob/main/site/install.sh);
it needs only `sh`, `curl` or `wget`, `tar`, and `sha256sum` or `shasum`.

### By hand

The same steps without the script. This downloads the latest release,
checks its SHA-256, and installs `ganymede` into `~/.local/bin`:

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

Set `VERSION=0.1.0` instead of the first line to pin a release.

### Verify a download

The `.sha256` files catch a damaged or truncated download. They come
from the same place as the archive, so they cannot show that the
archive is the one this repository built. Each release archive also
carries a signed build provenance attestation, made by the release
workflow from the tagged commit. With the [GitHub CLI](https://cli.github.com):

```sh
gh attestation verify "$NAME.tar.gz" --repo tlockney/ganymede
```

It succeeds only for an archive built by `tlockney/ganymede`'s own
release workflow, and prints the tag and commit it was built from.

### Platform notes

- **Linux** binaries are built against glibc and need 2.35 or newer
  (Ubuntu 22.04, Debian 12, Fedora 36, or later). On a musl system such
  as Alpine, or an older glibc, the binary fails to start with a bare
  `not found` (the missing piece is the glibc loader, not `ganymede`).
  There is no musl build yet; install [from source](#from-source)
  instead.
- **macOS** binaries are not signed or notarized. `curl`, and so the
  install script, doesn't mark files as quarantined, and they run as is.
  An archive downloaded with a browser is quarantined, and Gatekeeper
  refuses to open the binary ("cannot be opened because the developer
  cannot be verified"). Verify the download as above, then clear the
  flag before the first run:

  ```sh
  xattr -d com.apple.quarantine ganymede
  ```

  Or, after the first refusal, allow it under System Settings, Privacy &
  Security.

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
