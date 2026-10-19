# Install

<!--
Every command on this page is one that CI's `workstation` job runs, through
scripts/test-workstation.sh, on ubuntu-latest and macos-latest with Python
hidden from PATH. Change the page and the script together. Where they
differ:

- install.sh: the script fetches it from a file:// copy of a release built
  from the PR (`curl ... file://.../install.sh | sh -s -- --version X`),
  since the PR's binary isn't on GitHub yet.
- `ganymede lab`: the script passes `--no-browser`; the runners have no
  desktop browser to open, and Playwright opens the printed URL instead.
- Step 6: the script writes the cell to first.ipynb instead of typing it in
  JupyterLab, and runs it with `ganymede execute first.ipynb --inplace`
  (step 7). Playwright separately runs jupyter.display cells in JupyterLab.
- VS Code itself isn't run; validation/vscode.cjs replays the Jupyter
  extension's remote-server requests against `ganymede serve`.
- `ganymede lab remove 4.6.4`: the script takes the version from
  `ganymede lab list`.
- Not run by CI, and why:
  - `ganymede console -k cdp-node` is interactive.
  - `ganymede lab install --version`: a second ~18 MB download per run.
    tests/lab.rs covers installing by version.
  - Other ways to install: building from source, the by-hand download,
    and `gh attestation verify`. scripts/test-install.sh tests install.sh
    itself against a fake release.
  - Troubleshooting mentions `xattr`, `ldd --version`, and HTTPS_PROXY in
    prose only: they apply to machines the runners aren't (a browser
    download on macOS, an old glibc, a proxy).
-->

Ganymede is one binary, `ganymede`. It needs no Python and no Jupyter
install: kernels come from kernelspecs, which each kernel's own installer
writes.

These steps take a Linux or macOS machine with no Python and no Jupyter
to JupyterLab running TypeScript notebooks. You need a terminal, `curl`,
and about 300 MB of disk, most of it for Node.
CI runs the same commands on every pull request, on Linux and macOS, with
Python hidden from `PATH`
([`scripts/test-workstation.sh`](../scripts/test-workstation.sh)).
[Other ways to install](#other-ways-to-install) covers the script's
options, the download by hand, building from source, and Windows.

## 1. Install Ganymede

```sh
curl -fsSL https://ganymede-server.com/install.sh | sh
```

The script downloads the latest release for your platform, checks its
SHA-256, and installs `ganymede` into `~/.local/bin`. If Node 24 or later
is already installed, it also does step 2. If `~/.local/bin` isn't on your
`PATH`, it prints the line to add to your shell profile.
[Install script options](#install-script-options) lists its flags, and
[By hand](#by-hand) has the same steps without it.

Check it with the built-in `fake` kernel, which needs nothing else:

```sh
ganymede --version
ganymede run -k fake 'hello'     # prints "echo: hello" and "HELLO"
```

## 2. Install Node and cdp-kernel

[cdp-kernel](https://cdp-kernel.org) is the kernel that runs TypeScript
cells. It needs Node 24 or later. Check what you have:

```sh
node --version
```

If that fails or prints a version below 24, install Node from
[nodejs.org](https://nodejs.org/en/download), which also lists package
managers and version managers (nvm, fnm, and others). Then install the
kernel and register it:

```sh
npm install -g @tlockney/cdp-kernel
cdp-kernel install --runtime node
```

`cdp-kernel install` writes a kernelspec named `cdp-node` into Jupyter's
kernels directory (see [Kernel not listed](#kernel-not-listed)). cdp-kernel
also runs notebooks on Deno, Bun, workerd, and celld, one kernelspec per
runtime (`--runtime deno` and so on); its
[install docs](https://cdp-kernel.org/docs/install) cover what each needs.
Any other Jupyter kernel works the same way, ipykernel included, once its
kernelspec is installed.

## 3. Check the kernel

```sh
ganymede kernelspec list
ganymede run -k cdp-node 'const x: number = 2; x * 21'
```

The list has a `cdp-node` line, and the second command prints `42`. If
`cdp-node` is missing, see [Kernel not listed](#kernel-not-listed).

## 4. Install JupyterLab's frontend

```sh
ganymede lab install
ganymede lab list
ganymede lab extension list
```

`ganymede lab install` downloads the `jupyterlab` wheel from PyPI (about
18 MB), checks its SHA-256, and unpacks only the browser files: `static/`,
`schemas/`, `themes/`, and JupyterLab's BSD-3-Clause `LICENSE`. No Python
runs. They go in `ganymede/lab/<version>/` under your local data
directory: `~/.local/share` (or `$XDG_DATA_HOME`) on Linux,
`~/Library/Application Support` on macOS, or `$GANYMEDE_DATA_DIR` when that
is set. The command prints the directory.

It installs JupyterLab 4.6.4 by default; `--version X.Y.Z` picks another,
and `--force` reinstalls. It also installs the frontend halves of
`jupyterlab_widgets` and `anywidget`, so widgets (cdp-kernel's
`jupyter.widget`) work; `--no-default-extensions` skips them, and
`ganymede lab extension install <package>` adds other prebuilt extensions
from PyPI. [JupyterLab and VS Code](frontends.md#extensions) has the
details.

## 5. Start JupyterLab

Run it in the directory that holds, or will hold, your notebooks:

```sh
mkdir -p ~/notebooks && cd ~/notebooks
ganymede lab
```

It prints a URL such as
`http://127.0.0.1:8888/lab?token=2adb019db80b4948942534432a839642` and
opens it in your browser (`--no-browser` only prints it). The token is a
new random string each start; opening the URL with it logs the browser in
with a cookie, so the page keeps working after the token leaves the
address bar. If you open `/lab` without it, the page asks for the token.
The server listens on `127.0.0.1` only. Ctrl-C stops it: open requests
get 3 seconds to finish, and a second Ctrl-C stops it at once.

## 6. Your first notebook

In the launcher, under **Notebook**, pick **TypeScript (cdp-kernel,
node)**. Paste this into the first cell and press **Shift-Enter**:

```ts
import { format } from "npm:date-fns@4";

const day: string = format(new Date(2026, 0, 2), "EEEE, MMMM d");
await jupyter.display(jupyter.md`**${day}** is ${day.length} characters long.`);
```

The output is "**Friday, January 2** is 17 characters long.", rendered as
Markdown. The `npm:` import downloads `date-fns` from the npm registry on
first use, with no `npm install`, and `jupyter.display` shows a value
without ending the cell. Save the notebook as `first.ipynb` (**File › Save
Notebook As…**). cdp-kernel's
[Getting started](https://cdp-kernel.org/docs/getting-started) continues
from here.

## 7. Other frontends

### VS Code

VS Code connects to `ganymede serve`, the same server without JupyterLab's
page and without opening a browser:

```sh
cd ~/notebooks
ganymede serve
```

It prints a URL such as `http://127.0.0.1:8888/?token=…`. In VS Code,
install the [Jupyter extension](https://marketplace.visualstudio.com/items?itemName=ms-toolsai.jupyter),
open a notebook, click **Select Kernel**, then **Select Another Kernel…** ›
**Existing Jupyter Server**, and paste the URL, token included. Pick
`cdp-node` from the list.

The Python extension is not needed for this. The Jupyter extension's
manifest declares no extension dependencies, and the extensions it
installs with itself are Jupyter Keymap, Notebook Renderers, Cell Tags,
and Slide Show
([package.json](https://github.com/microsoft/vscode-jupyter/blob/main/package.json)
on the main branch, version 2026.6.1, checked October 2026). Its
[README](https://github.com/microsoft/vscode-jupyter#readme) asks for the
Python extension only under "Working with Python", and VS Code's
[kernel management docs](https://code.visualstudio.com/docs/datascience/jupyter-kernel-management)
describe the Existing Jupyter Server flow without it. CI replays the
extension's remote-server requests against `ganymede serve`; it does not
run VS Code itself.

### The terminal

`ganymede execute` runs a notebook top to bottom and saves the outputs,
like `jupyter execute`; `ganymede console` is an interactive prompt:

```sh
ganymede execute first.ipynb --inplace
ganymede console -k cdp-node
```

[Getting started](getting-started.md) covers these and the rest of the
command line.

## 8. Update and uninstall

The steps above put files in these places:

![What the install puts where. The ganymede binary goes in ~/.local/bin. It finds kernels in the Jupyter data dir (~/.local/share/jupyter, or ~/Library/Jupyter on macOS), where cdp-kernel install writes kernels/cdp-node/kernel.json and where notebook signatures are kept; that kernel.json's argv starts cdp-kernel, installed with npm on Node 24 or later. ganymede lab install puts JupyterLab and its extensions in Ganymede's data dir. The Jupyter config dir, ~/.jupyter, holds an optional ganymede.toml and JupyterLab's settings and workspaces, and the runtime dir holds each running kernel's connection file and the cookie secret.](images/architecture/install.svg)

To update Ganymede, run the install command from step 1 again; it replaces
the binary in place. To update cdp-kernel, rerun step 2's `npm install -g`
and `cdp-kernel install` lines. A new Ganymede release can default to a
newer JupyterLab; run `ganymede lab install` again to get it. Older
versions stay until you remove them.

To uninstall, remove each piece:

```sh
ganymede lab list                                             # installed versions
ganymede lab remove 4.6.4
ganymede lab extension remove anywidget jupyterlab-widgets
cdp-kernel uninstall cdp-node
npm uninstall -g @tlockney/cdp-kernel
rm ~/.local/bin/ganymede
```

`cdp-kernel uninstall` (cdp-kernel 0.6.0 and later) removes only the
kernelspecs cdp-kernel wrote, and `cdp-kernel list` shows them; see its
[uninstall docs](https://cdp-kernel.org/docs/install#uninstall). With an
older cdp-kernel, use `ganymede kernelspec remove cdp-node` instead.

This leaves the `ganymede` data directory from step 4, now empty but for
bookkeeping; Jupyter's `~/.jupyter` (JupyterLab settings and workspaces);
the Jupyter data directory (`~/.local/share/jupyter` or
`~/Library/Jupyter`, which holds the notebook-signing key); and
cdp-kernel's caches, which its uninstall docs list. Delete them if nothing
else uses them.

## 9. Troubleshooting

### Port in use

`ganymede lab` tries port 8888 and moves to the next free port, up to 50
more, so read the port from the URL it prints. `ganymede serve` does not
move; it fails with `Address already in use`. Pass `--port` with another
number, or `--port 0` for any free port.

### macOS blocks the binary

macOS marks files downloaded by a browser with the
`com.apple.quarantine` attribute, and Gatekeeper refuses to run them
("cannot be opened because the developer cannot be verified") because the
binaries are not signed or notarized. `curl` does not set the attribute,
so the install script and the by-hand steps are not affected. If you
downloaded the archive in a browser, [verify it](#verify-a-download), then
clear the attribute before the first run:

```sh
xattr -d com.apple.quarantine ganymede
```

Or, after the first refusal, allow it under System Settings, Privacy &
Security.

### glibc too old

The Linux binaries are built on Ubuntu 22.04 against glibc, so they need
glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36, or later).
On an older system, running `ganymede` fails with
``version `GLIBC_2.xx' not found``; `ldd --version` shows the glibc you
have. On a musl system such as Alpine it fails with a bare `not found`
(the missing piece is the glibc loader, not `ganymede`). The install
script checks for both and stops before downloading. There is no musl
build yet; [build from source](#from-source) instead.

### Kernel not listed

`ganymede kernelspec list` reads kernelspecs from Jupyter's data
directories:

| Platform | Kernels directory |
| --- | --- |
| Linux | `~/.local/share/jupyter/kernels` (or `$XDG_DATA_HOME/jupyter/kernels`) |
| macOS | `~/Library/Jupyter/kernels` |
| Any | `$JUPYTER_DATA_DIR/kernels`, then each entry of `$JUPYTER_PATH`, when set |

then `/usr/local/share/jupyter/kernels` and `/usr/share/jupyter/kernels`.
`ganymede kernelspec paths` prints the directories it searched, and
`ganymede info` prints the data directories in use:

```sh
ganymede kernelspec paths
ganymede info
```

If `cdp-node` isn't in any of them, `cdp-kernel install` ran with a
different `HOME`, `JUPYTER_DATA_DIR`, or `XDG_DATA_HOME`; run it again in
the same environment as `ganymede`. If the shell says `cdp-kernel: command
not found` after `npm install -g`, npm's global `bin` directory isn't on
your `PATH`; `npm prefix -g` prints its parent.

### Behind a proxy

`ganymede lab install` and `ganymede lab extension install` use
`HTTPS_PROXY` (or `ALL_PROXY`) from the environment and trust the
certificates in the system's trust store, so a proxy's CA installed there
works. npm reads the same variable, or its own `https-proxy` setting, for
`npm install -g` and for the packages cdp-kernel fetches for `npm:`
imports. Set it before running them, for example
`export HTTPS_PROXY=http://proxy.example.com:3128`. The install script
uses `curl`, or `wget` without it, and both read `https_proxy` in lower
case.

## Other ways to install

### Install script options

Options go after `sh -s --`:

| Option | Effect |
| --- | --- |
| `--version X.Y.Z` | Install that release instead of the latest. |
| `--bin-dir DIR` | Install into `DIR` instead of `~/.local/bin`. |
| `--no-kernel` | Skip cdp-kernel. |
| `--runtime NAME` | cdp-kernel's target runtime: `node` (default), `deno`, `bun`, `workerd`, or `celld`. |

```sh
curl -fsSL https://ganymede-server.com/install.sh | sh -s -- --version 0.1.0 --no-kernel
```

If the cdp-kernel step fails, the script says so and still exits 0, since
`ganymede` is installed. The script is
[`site/install.sh`](../site/install.sh); it needs only `sh`, `curl` or
`wget`, `tar`, and `sha256sum` or `shasum`.

### By hand

Releases attach a prebuilt archive per platform: Linux (x86_64,
aarch64), macOS (aarch64, x86_64), and Windows (x86_64), each with a
`.sha256` beside it, `SHA256SUMS` for all of them, and a build
provenance attestation. Get one from the
[releases page](https://github.com/tlockney/ganymede/releases), or do what
the install script does. This downloads the latest release, checks its
SHA-256, and installs `ganymede` into `~/.local/bin`:

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

Set `VERSION=0.1.0` instead of the first line to pin a release. Windows
binaries are built and released but not tested end to end; see
[Known gaps](known-gaps.md#platforms).

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

### From source

With a Rust toolchain ([rustup](https://rustup.rs)):

```sh
cargo install --git https://github.com/tlockney/ganymede ganymede-cli
ganymede --version
```

From a checkout, `cargo install --path crates/ganymede-cli` does the same.
