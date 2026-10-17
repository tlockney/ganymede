# Getting started

This page assumes `ganymede` is [installed](install.md) along with at
least one kernel. The examples use `cdp-node` from cdp-kernel; substitute
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
```

## Serve the Jupyter API

`ganymede serve` serves the REST and websocket API that JupyterLab, VS
Code, and other Jupyter clients use:

```sh
ganymede serve --root-dir . --port 8888
```

It prints the URL, with its token, as the only line on stdout. The
[JupyterLab and VS Code](frontends.md) guide covers connecting a frontend.

Useful flags, each of which can also be set in a TOML file passed with
`--config`:

| Flag | Effect |
|---|---|
| `--root-dir` | Where notebooks and files are served from. Default: the current directory. |
| `--ip`, `--port` | Where to listen. `--port 0` picks a free port. |
| `--token` | The auth token. Unset generates one; an empty string disables auth. |
| `--default-kernel` | The kernelspec frontends offer first. |
| `--cull-idle-secs` | Shut down kernels idle this long with no clients. |
| `--static-dir` | Serve a frontend's static files; JupyterLab's bundle is recognized. Default: the newest JupyterLab from `ganymede lab install`. |
| `--lab-version` | Which installed JupyterLab to serve. |
| `--notary-dir` | Where the notebook-signing key and trusted signatures live. Default: the Jupyter data dir. |

## Embed it

The CLI is built on `ganymede-engine`, which an application can use
directly:

```rust
let engine = ganymede_engine::Engine::new();
let kernel = engine.start("cdp-node").await?;
let result = kernel.execute("1 + 1").await?;
println!("{}", result.text());
```
