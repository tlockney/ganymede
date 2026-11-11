# Ganymede

Ganymede is a Jupyter server written in Rust, for TypeScript notebooks on
[cdp-kernel](https://cdp-kernel.org), agent journals over
[MCP](mcp.md), and its own Mac, iPad and iPhone [apps](app.md). It finds
and launches kernels from installed kernelspecs, speaks the Jupyter
messaging protocol to them, reads and writes notebooks, and serves the
same REST and websocket API as `jupyter_server`, so JupyterLab and VS Code
connect to it as they would to Jupyter.

Other kernels work too: a Python kernel is one more installed kernelspec,
not something the server needs ([Other kernels](kernels.md)).

## Get started

- [Install](install.md): numbered steps from a clean Linux or macOS
  machine to JupyterLab running TypeScript notebooks, with no Python or
  Jupyter; also the binary by hand or from source.
- [Getting started](getting-started.md): run code, execute notebooks, and
  serve the API.
- [Notebooks as programs](programs.md): parameters in, named results
  out, from the CLI, CI, HTTP, links, Shortcuts and other notebooks.
- [JupyterLab and VS Code](frontends.md): point an existing frontend at
  Ganymede.
- [Ganymede's app](app.md): its own notebook app, in a browser and on the
  Mac, iPad and iPhone.
- [Agents over MCP](mcp.md): a coding agent journaling an analysis into
  a notebook, with `ganymede mcp` and the ganymede-journal plugin for
  Claude Code; [nb-cli](agents.md) for agents without MCP.
- [CLI and configuration](reference.md): every command, flag, config key
  and environment variable.
- [Architecture](architecture.md): how Ganymede compares to the Jupyter
  stack it replaces, and how its crates fit together.
- [Roadmap](roadmap.md): what is done and what comes next.

## Status

Early software. The protocol, kernel host, notebook format, executor,
contents, API, and server build and pass their tests, and an end-to-end
suite compares Ganymede against `jupyter execute` and `jupyter server` on
cdp-kernel and ipykernel. [Known gaps](known-gaps.md) lists what doesn't
work yet, the [Roadmap](roadmap.md) what is planned, and the
[development log](notes.md) what each step found.
