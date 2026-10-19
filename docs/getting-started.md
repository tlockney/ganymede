# Getting started

This page assumes `ganymede` and at least one kernel are installed;
[Install](install.md) walks through both, and JupyterLab, step by step.
The examples use `cdp-node` from cdp-kernel; substitute
any name `ganymede kernelspec list` shows. With no kernel installed,
`-k fake` uses a built-in in-process kernel for trying things out.

The commands fall into three groups: those that run code on a kernel
with no server, those that start a server for frontends, and those that
need no kernel at all.

![What each ganymede command takes and gives. Commands that run code on a kernel without a server: run takes code, a file or stdin and prints output; console is interactive; execute runs notebooks and writes their outputs; kernel starts a kernel and prints its connection file, which run --existing and attach reuse. Commands that start a server for frontends: serve prints a URL with its token; lab serves JupyterLab at /lab and ui serves ganymede-ui at /, each opened in the browser. Commands with no kernel: trust signs a notebook, nb clear removes outputs, and kernelspec, lab install and info manage what is installed.](images/architecture/cli.svg)

> [!NOTE]
> **Unreleased.** `ganymede ui` is available only from a build of `main`
> (HEAD), not in the latest release.

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

It exits 1 if a cell fails, so it works in scripts and CI. With `--json`,
a cell that could not finish (it timed out, or the kernel died) still gets
a line, with `"status": "incomplete"` and an `error`, so a script can tell
where it stopped. Input requests get an empty reply under `--json` and when
the code comes from stdin; empty stdin is an error.

Each `--json` line is one cell, with the outputs as a notebook stores
them. From `ganymede run -k fake --json hello raise`:

```json
{"cell":0,"status":"ok","execution_count":1,"outputs":[{"output_type":"stream","name":"stdout","text":"echo: hello\n"},{"output_type":"execute_result","execution_count":1,"data":{"text/plain":"HELLO"},"metadata":{}}]}
{"cell":1,"status":"error","execution_count":2,"outputs":[{"output_type":"error","ename":"FakeError","evalue":"raised","traceback":["\u001b[31mFakeError\u001b[0m: raised"]}],"ename":"FakeError","evalue":"raised"}
```

The [exit statuses](reference.md#exit-status) are the same for every
command.

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
With several notebooks, each one that fails, can't be read, or whose
kernel won't start is reported and the rest still run; the exit status is
1 if any failed. `jupyter execute`'s flag names `--kernel_name`,
`--startup_timeout`, `--skip-cells-with-tag` and `--allow-error-names` work
too.
`ganymede nb clear notebook.ipynb --inplace` removes outputs without a
kernel.

In CI, a notebook that fails fails the job. A GitHub Actions workflow
that runs every notebook in `notebooks/` on cdp-kernel:

```yaml
name: notebooks
on: [push, pull_request]
jobs:
  run:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: actions/setup-node@v5
        with:
          node-version: 24
      # Installs ganymede, and cdp-kernel since Node 24 is there.
      - run: curl -fsSL https://ganymede-server.com/install.sh | sh
      - run: echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: ganymede execute notebooks/*.ipynb
```

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

## Serve notebooks to a frontend

In the directory with your notebooks, as with `jupyter lab`:

```sh
ganymede lab install     # once: JupyterLab's frontend, from PyPI, no Python
ganymede lab             # JupyterLab, opened in the browser
ganymede serve           # the same server for VS Code and other clients
```

`serve` prints the URL, with its token, as the only line on stdout.
[JupyterLab and VS Code](frontends.md) covers connecting a frontend and
installing extensions, and [Ganymede's app](app.md) its own notebook app.
The server's flags, config file and logging are in
[CLI and configuration](reference.md#server-flags).

The server runs code for anyone who holds its token. It listens on
loopback only unless `--ip` says otherwise; [Security](security.md)
covers reaching it from other machines safely.

## Shell completions

```sh
ganymede completions bash > ~/.local/share/bash-completion/completions/ganymede
ganymede completions zsh > ~/.zfunc/_ganymede    # with ~/.zfunc on fpath
ganymede completions fish > ~/.config/fish/completions/ganymede.fish
```

`powershell` and `elvish` work too.

## Embed it

The CLI is built on `ganymede-engine`, which an application can use
directly:

```rust
let engine = ganymede_engine::Engine::new();
let kernel = engine.start("cdp-node").await?;
let result = kernel.execute("1 + 1").await?;
println!("{}", result.text());
```

[Embedding](embedding.md) has the rest of the engine's API, streaming
output and stdin, builds without ZeroMQ or subprocesses, and the C ABI
that runs the server inside an app.
