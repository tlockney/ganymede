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
aarch64), macOS (aarch64, x86_64), and Windows (x86_64). Each holds the
`ganymede` binary and has a `.sha256` beside it; `SHA256SUMS` covers them
all. Download one from the
[releases page](https://github.com/tlockney/ganymede/releases), check it,
and put `ganymede` on your `PATH`:

```sh
shasum -a 256 -c ganymede-<version>-<target>.tar.gz.sha256
tar xzf ganymede-<version>-<target>.tar.gz
mv ganymede-<version>-<target>/ganymede ~/.local/bin/
```

The macOS binary isn't signed or notarized yet. Gatekeeper quarantines it
on download; clear that with `xattr -d com.apple.quarantine ganymede`.

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
