# Getting started

This page assumes `ganymede` is [installed](install.md) along with at
least one kernel; [Install on a workstation](workstation.md) sets up both,
and JupyterLab, step by step. The examples use `cdp-node` from cdp-kernel; substitute
any name `ganymede kernelspec list` shows. With no kernel installed,
`-k fake` uses a built-in in-process kernel for trying things out.

## Find your kernels

```sh
ganymede kernelspec list    # name, language, and where each spec lives
ganymede kernelspec paths   # the directories searched
ganymede info               # versions and the directories in use
```

`ganymede kernelspec install <dir>` and `ganymede kernelspec remove <name>`
manage specs as `jupyter kernelspec` does. Add `--kernelspec-root <dir>` to
any command to search another directory first.

## Run code

`ganymede run` starts a kernel, runs each argument as one cell, prints the
output, and shuts the kernel down:

```sh
ganymede run -k cdp-node 'const x = 2; x * 21'
ganymede run -k cdp-node -f script.ts          # a file as one cell
echo 'console.log("hi")' | ganymede run -k cdp-node
ganymede run -k cdp-node --json 'x'            # one JSON line per cell
```

It exits 1 if a cell fails, so it works in scripts and CI.

`ganymede console -k cdp-node` is an interactive console with completion,
multiline input, and Ctrl-C to interrupt.

## Execute notebooks

`ganymede execute` runs a notebook top to bottom as `jupyter execute`
(nbclient) does, and writes the same outputs, execution counts, and
timings:

```sh
ganymede execute notebook.ipynb                  # check that it runs
ganymede execute notebook.ipynb -o out.ipynb     # save the result
ganymede execute notebook.ipynb --inplace
```

The kernel comes from the notebook's metadata unless you pass `-k`.
Execution stops at the first error unless you pass `--allow-errors`.
`ganymede nb clear notebook.ipynb --inplace` removes outputs without a
kernel.

## Trust a notebook

JupyterLab sanitizes HTML and skips JavaScript outputs of a notebook it
did not see produced, so a notebook from elsewhere shows them only once
it is trusted. The server trusts a notebook when you save it with outputs
from this session; for one you got another way:

```sh
ganymede trust notebook.ipynb                    # like `jupyter trust`
```

Signatures are kept where Jupyter keeps them (`notebook_secret` and
`nbsignatures.db` in the Jupyter data dir), so a notebook trusted by one
is trusted by the other. `--reset` forgets every signature.

## Keep a kernel running

`ganymede kernel` starts a kernel and leaves it running until Ctrl-C,
printing its connection file, as `jupyter kernel` does. Other tools attach
to it by that file:

```sh
ganymede kernel -k cdp-node
ganymede run --existing kernel-<id>.json 'x'
ganymede attach kernel-<id>.json 'x'      # the same, waiting at most 60 s per cell
```

`ganymede run --existing` with no file attaches to the newest kernel in
the Jupyter runtime dir. A kernel that was attached to keeps running when
the command ends.

## Start JupyterLab

In the directory with your notebooks, as with `jupyter lab`:

```sh
ganymede lab install     # once: JupyterLab's frontend, from PyPI, no Python
ganymede lab
```

`ganymede lab` is `ganymede serve` with the installed JupyterLab. It
prints the URL of `/lab`, with its token, and opens it in the default
browser (`open` on macOS, `xdg-open` on Linux); `--no-browser` only
prints it. The token in the URL logs the browser in. Port 8888 is tried
first, then each next port while it is taken, up to 50 more. It takes
`serve`'s flags below, and fails, naming `ganymede lab install`, when no
JupyterLab is installed.

```sh
ganymede lab list                          # installed versions; --json too
ganymede lab remove 4.6.4
ganymede lab install --no-default-extensions   # without the widget extensions
ganymede lab extension install jupyterlab-execute-time
ganymede lab extension list                # --json too
ganymede lab extension disable jupyterlab-execute-time   # and `enable`
ganymede lab extension remove jupyterlab-execute-time
ganymede lab clean                         # leftovers of interrupted installs
ganymede lab uninstall                     # every version and extension
```

Everything `lab install` adds lives in Ganymede's data dir,
`$GANYMEDE_DATA_DIR` when set; `ganymede info` prints where. `disable`
writes `labconfig/page_config.json` in the Jupyter config dir, as
`jupyter labextension disable` does, and the next page load picks it up.
See [JupyterLab and VS Code](frontends.md#extensions) for how extensions
are found and installed.

## Shell completions

```sh
ganymede completions bash > ~/.local/share/bash-completion/completions/ganymede
ganymede completions zsh > ~/.zfunc/_ganymede    # with ~/.zfunc on fpath
ganymede completions fish > ~/.config/fish/completions/ganymede.fish
```

`powershell` and `elvish` work too.

## Serve the Jupyter API

`ganymede serve` serves the REST and websocket API that JupyterLab, VS
Code, and other Jupyter clients use:

```sh
ganymede serve --root-dir . --port 8888
```

It prints the URL, with its token, as the only line on stdout. The
[JupyterLab and VS Code](frontends.md) guide covers connecting a frontend.

Useful flags, each of which can also be set in a TOML file passed with
`--config` (an unknown key in the file is an error):

| Flag | Effect |
|---|---|
| `--root-dir` | Where notebooks and files are served from. Default: the current directory. |
| `--ip`, `--port` | Where to listen. `--port 0` picks a free port. |
| `--token` | The auth token. Unset generates one; an empty string disables auth. |
| `--allow-remote-access` | Accept requests addressed to any host name. Without it, a server on a loopback address only answers requests whose `Host` is a loopback address or a name in `local_hostnames` (default `localhost`). |
| `--default-kernel` | The kernelspec frontends offer first. |
| `--cull-idle-secs` | Shut down kernels idle this long with no clients. |
| `--static-dir` | Serve a frontend's static files; JupyterLab's bundle is recognized. Default: the newest JupyterLab from `ganymede lab install`. |
| `--lab-version` | Which installed JupyterLab to serve. |
| `--notary-dir` | Where the notebook-signing key and trusted signatures live. Default: the Jupyter data dir. |
| `--base-url` | A prefix for serving behind a reverse proxy, e.g. `/jupyter/`. |
| `--checkpoint-dir` | Keep checkpoints here instead of `.ipynb_checkpoints` beside each file. |
| `--allow-origin` | An origin, besides the server's own, allowed to use the API and websockets. Repeatable. |
| `--startup-timeout` | Seconds a new kernel has to answer (`startup_timeout_secs`). Default 60. |
| `-q`, `--quiet` | Discard kernels' own stdout and stderr (`quiet`). Cell output is unaffected. |

`serve` and `lab` log at info level on stderr; `-v` raises it to debug, and
`RUST_LOG` overrides both.

## Embed it

The CLI is built on `ganymede-engine`, which an application can use
directly:

```rust
let engine = ganymede_engine::Engine::new();
let kernel = engine.start("cdp-node").await?;
let result = kernel.execute("1 + 1").await?;
println!("{}", result.text());
```
