# Ganymede

Ganymede is a Jupyter server written in Rust, with no Python anywhere in
the product. It finds and launches kernels from installed kernelspecs,
speaks the Jupyter messaging protocol to them, reads and writes notebooks,
and serves the same REST and websocket API as `jupyter_server`, so
JupyterLab and VS Code connect to it as they would to Jupyter.

The first target kernel is [cdp-kernel](https://cdp-kernel.org), for
TypeScript notebooks. Python kernels still work: a Python kernel is one
more installed kernelspec, not something the server needs.

## Where to start

- [Install](install.md): numbered steps from a clean Linux or macOS
  machine to JupyterLab running TypeScript notebooks, with no Python or
  Jupyter; also the binary by hand or from source.
- [Getting started](getting-started.md): run code, execute notebooks, and
  serve the API.
- [JupyterLab and VS Code](frontends.md): point an existing frontend at
  Ganymede.
- [Architecture](architecture.md): how Ganymede compares to the Jupyter
  stack it replaces, and how its crates fit together.
- [Roadmap](roadmap.md): what is done and what comes next.

## Status

Early software. The protocol, kernel host, notebook format, executor,
contents, API, and server build and pass their tests, and an end-to-end
suite compares Ganymede against `jupyter execute` and `jupyter server` on
cdp-kernel and ipykernel. The [development log](notes.md) records what
each step found.
