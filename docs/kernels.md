# Other kernels

Ganymede has no language of its own. It starts whatever kernels are
installed, from the same kernelspecs `jupyter_server` reads, so any
Jupyter kernel works once its kernelspec is where Ganymede looks.
[Install](install.md) sets up cdp-kernel for TypeScript; this page covers
Python through ipykernel, then other kernels in brief.

Ganymede itself stays Python-free. Python is a kernel you choose to
install, in an environment you manage, and nothing else on this page
needs it.

## Python (ipykernel)

ipykernel is the Python kernel JupyterLab and VS Code use. CI runs
Ganymede's end-to-end suite against it (ipykernel 7.4) on Linux and
macOS, alongside cdp-kernel.

### 1. Install ipykernel in an environment

Use the Python environment your notebooks should run in, with the
packages they import. With the standard library's `venv` and pip:

```sh
python3 -m venv ~/venvs/myproject
~/venvs/myproject/bin/pip install ipykernel
```

With uv, `uv venv ~/venvs/myproject` and `uv pip install --python
~/venvs/myproject ipykernel`. With conda, `conda create -n myproject
python ipykernel`.

Jupyter itself (`jupyter`, `notebook`, `jupyterlab`) isn't needed; only
`ipykernel` and what your code imports.

### 2. Register the kernelspec

Run `ipykernel install` with that environment's Python:

```sh
~/venvs/myproject/bin/python -m ipykernel install --user \
  --name myproject --display-name "Python (myproject)"
```

With conda, activate the environment first (`conda activate myproject`)
and run `python -m ipykernel install --user ...` the same way.

`--user` writes `kernels/myproject/kernel.json` into your user data
directory (see [How Ganymede finds kernels](#how-ganymede-finds-kernels)).
Its `argv` names the environment's Python by absolute path, so the kernel
runs in that environment whatever is on `PATH`:

```json
{
  "argv": ["/home/you/venvs/myproject/bin/python", "-Xfrozen_modules=off",
           "-m", "ipykernel_launcher", "-f", "{connection_file}"],
  "display_name": "Python (myproject)",
  "language": "python",
  "metadata": { "debugger": true, "supported_encryption": ["curve"] },
  "kernel_protocol_version": "5.5"
}
```

`--name` is the name you pass to `-k` and the one notebooks record in
`metadata.kernelspec.name`; it must match `[a-z0-9._-]+`. Give each
environment its own name. Without `--name` it is `python3`, and a second
environment registered the same way replaces the first.

### 3. Check that it runs

```sh
ganymede kernelspec list
ganymede run -k myproject 'import sys; print(sys.executable); 6 * 7'
```

The list has a `myproject` line, and `run` prints the environment's
Python path and `42`. The kernel then shows in JupyterLab's launcher, VS
Code's kernel picker, and the Mac app, as with any other kernelspec.

ipykernel 7 prints a warning on start that it is "running over TCP
without encryption". Ganymede binds kernels to `127.0.0.1` and doesn't
yet set up CurveZMQ ([Known gaps](known-gaps.md)); `-q` hides kernel
stderr from `ganymede run`.

On Windows, interrupting an ipykernel cell doesn't work yet: ipykernel
there waits for an event that `jupyter_client` sets up, and Ganymede
supports signal and message interrupts only
([Replacing Python](python-free.md)).

### A virtual environment's own kernelspec

`pip install ipykernel` also drops a `python3` kernelspec inside the
environment, at `<venv>/share/jupyter/kernels/python3`. `jupyter_client`
finds it because it searches Python's `sys.prefix`; Ganymede doesn't,
since it has no Python to ask. That spec's `argv` starts with plain
`python`, which `jupyter_client` swaps for its own interpreter and
Ganymede runs from `PATH`.

To use it anyway, add the directory as a root and put the environment's
`bin` first on `PATH`, for example by activating it:

```sh
source ~/venvs/myproject/bin/activate
ganymede --kernelspec-root "$VIRTUAL_ENV/share/jupyter" run -k python3 '1 + 1'
```

With the environment inactive, `python` is whatever else is on `PATH`,
and the kernel fails with `No module named ipykernel_launcher`. The
`--user --name` install above avoids both steps.

### Remove it

```sh
ganymede kernelspec remove myproject
```

This deletes the kernelspec directory, not the environment.

## How Ganymede finds kernels

![Where Ganymede looks for kernelspecs. It searches roots in order and the first spec with a given name wins: roots given with --kernelspec-root or kernelspec_roots, then each $JUPYTER_PATH entry, then the user data dir ($JUPYTER_DATA_DIR, ~/.local/share/jupyter on Linux, ~/Library/Jupyter on macOS), where python -m ipykernel install --user writes a spec whose argv is the environment's python by absolute path, then /usr/local/share/jupyter and /usr/share/jupyter, where ipykernel install --prefix /usr/local and distro packages write. A virtual environment's own share/jupyter, where pip install ipykernel drops a python3 spec whose argv is plain python, is not searched unless it is added as a root.](images/kernels/discovery.svg)

A kernelspec is a directory holding `kernel.json` (and optional logos).
Ganymede reads `<root>/kernels/<name>/kernel.json` under each root, in
this order:

1. Each `--kernelspec-root DIR`, or `kernelspec_roots` in
   [`ganymede.toml`](reference.md) for the server.
2. Each entry of `$JUPYTER_PATH`.
3. The user data directory: `$JUPYTER_DATA_DIR` when set, otherwise
   `~/.local/share/jupyter` (or `$XDG_DATA_HOME/jupyter`) on Linux,
   `~/Library/Jupyter` on macOS, `%APPDATA%\jupyter` on Windows.
4. `/usr/local/share/jupyter` and `/usr/share/jupyter`
   (`%PROGRAMDATA%\jupyter` on Windows).

This is `jupyter_client`'s order without its `sys.prefix` root. Names are
compared case-insensitively, and the first root with a name wins, so a
`python3` under `--kernelspec-root` hides one in the user directory.
`ganymede kernelspec paths` prints the directories searched, and `ganymede
kernelspec list` shows where each spec came from.

In each `argv`, Ganymede replaces `{connection_file}` with the file it
writes for the kernel and `{resource_dir}` with the kernelspec's
directory, and adds the spec's `env` to the kernel's environment. A bare
command such as `python` or `R` is looked up on `PATH`, which for the Mac
app is your login shell's.

To install a kernelspec directory by hand, without the kernel's own
installer:

```sh
ganymede kernelspec install path/to/spec --name NAME
```

It copies the directory into the user data directory (`--prefix PREFIX`
puts it under `PREFIX/share/jupyter` instead).

## More kernels

Each kernel below ships its own kernelspec. Ganymede is tested on
cdp-kernel and ipykernel only; the others speak the same protocol and
should work, and a difference from `jupyter_server` is a bug worth
[reporting](https://github.com/tlockney/ganymede/issues). Where an
installer calls the `jupyter` command, which Ganymede doesn't need, use
`ganymede kernelspec install` on the spec directory instead.

| Language | Kernel | Install the kernelspec |
| --- | --- | --- |
| TypeScript, JavaScript | [cdp-kernel](https://cdp-kernel.org) | `cdp-kernel install --runtime node` ([Install](install.md#_2-install-node-and-cdp-kernel)) |
| TypeScript, JavaScript | [Deno](https://docs.deno.com/runtime/reference/cli/jupyter/) | `deno jupyter --install` |
| R | [IRkernel](https://irkernel.github.io) | `IRkernel::installspec()` calls `jupyter`; without it, `ganymede kernelspec install "$(Rscript -e 'cat(system.file("kernelspec", package = "IRkernel"))')" --name ir` |
| Julia | [IJulia](https://github.com/JuliaLang/IJulia.jl) | `using Pkg; Pkg.add("IJulia")` writes a spec per Julia version |
| Rust | [evcxr](https://github.com/evcxr/evcxr) | `cargo install --locked evcxr_jupyter && evcxr_jupyter --install` |
| Go | [gophernotes](https://github.com/gopherdata/gophernotes) | `go install`, then copy its `kernel` directory in with `ganymede kernelspec install` |
| C++ | [xeus-cpp](https://github.com/compiler-research/xeus-cpp) | Conda package; its specs land in `$CONDA_PREFIX/share/jupyter`, so pass that as `--kernelspec-root` |
| Scala | [Almond](https://almond.sh) | The Almond launcher's `--install` |

The Jupyter project keeps a longer
[list of kernels](https://github.com/jupyter/jupyter/wiki/Jupyter-kernels).

If a kernel doesn't appear in `ganymede kernelspec list`, see
[Kernel not listed](install.md#kernel-not-listed).
