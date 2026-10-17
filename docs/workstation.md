# Install on a workstation (no Jupyter needed)

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
  - `cdp-kernel uninstall` and `cdp-kernel list` are not in a cdp-kernel
    release yet (0.5.0 lacks them); the page gives `ganymede kernelspec
    remove`, which CI runs.
  - `ganymede lab install --version`: a second ~18 MB download per run.
    tests/lab.rs covers installing by version.
  - The by-hand download is install.md's, which this page links.
  - Troubleshooting mentions `xattr`, `ldd --version`, and HTTPS_PROXY in
    prose only: they apply to machines the runners aren't (a browser
    download on macOS, an old glibc, a proxy).
-->

These steps take a Linux or macOS machine with no Python and no Jupyter
to JupyterLab running TypeScript notebooks. You need a terminal, `curl`,
and about 300 MB of disk, most of it for Node.
CI runs the same commands on every pull request, on Linux and macOS, with
Python hidden from `PATH`
([`scripts/test-workstation.sh`](../scripts/test-workstation.sh)).

[Install](install.md) is the reference for the same steps: building from
source, the install script's options, and other kernels.

## 1. Install Ganymede

```sh
curl -fsSL https://ganymede-server.com/install.sh | sh
```

The script downloads the latest release for your platform, checks its
SHA-256, and installs `ganymede` into `~/.local/bin`. If Node 24 or later
is already installed, it also does step 2. If `~/.local/bin` isn't on your
`PATH`, it prints the line to add to your shell profile.

To download and check the archive yourself instead, follow
[Install › By hand](install.md#by-hand).

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
The server listens on `127.0.0.1` only. Ctrl-C stops it.

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
ganymede kernelspec remove cdp-node
npm uninstall -g @tlockney/cdp-kernel
rm ~/.local/bin/ganymede
```

cdp-kernel releases after 0.5.0 also have `cdp-kernel list` and
`cdp-kernel uninstall cdp-node`, which remove only the kernelspecs
cdp-kernel wrote; see its
[uninstall docs](https://cdp-kernel.org/docs/install#uninstall).

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
because the binaries are not notarized. `curl` does not set the attribute,
so the install script and the by-hand steps are not affected. If you
downloaded the archive in a browser, clear it before the first run:
`xattr -d com.apple.quarantine ganymede`.

### glibc too old

The Linux binaries are built on Ubuntu 22.04 against glibc, so they need
glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36, or later).
On an older system, running `ganymede` fails with
``version `GLIBC_2.xx' not found``; `ldd --version` shows the glibc you
have. Build from source instead, as in [Install › From source](install.md#from-source).

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
