# jupyter-core-rs

A sketch of a Jupyter server in Rust with no Python anywhere in the
product: a kernel-agnostic kernel host, the messaging protocol, the server
API, and eventually a web presentation layer. First target kernel is
[cdp-kernel](https://cdp-kernel.org) for TypeScript notebooks.

- [`sketch.md`](sketch.md): components, what already exists in Rust, the
  parts that are usually missed, and a suggested order of work.
- [`notes.md`](notes.md): findings and open questions.

Section 6 of the sketch reads the server against the
[ts-on-ipados](../ts-on-ipados/) plan, where it is the remote tier.
Section 7 gives the module seams, the crate layout, and which modules
each deployment profile (server, desktop, iPad) uses.

## Status

Design sketch. Nothing built.

## Findings

- The server half (protocol, kernel lifecycle, nbformat, REST and
  websocket server, CRDT document) is well-specified and suits Rust.
- The frontend half does not: editor, math, output renderers, and widgets
  are JavaScript. A TypeScript UI over a Rust server is the pragmatic shape.
- The runtimed crates (`jupyter-protocol`, `runtimelib`, `nbformat`)
  already cover protocol, kernel client, and file format. No Rust
  equivalent of `jupyter_server` exists.
- Python enters the reference stack around the protocol, not in it:
  kernelspec paths, the `python3` default, ipywidgets, trust storage,
  nbconvert, config, and the extension system. Each has a replacement;
  see section 3 of the sketch.
- The server alone, with VS Code's Jupyter extension as the frontend and
  cdp-kernel as the kernel, is a complete Python-free setup before any UI
  work.
- The same modules compose into three profiles. The iPad profile has no
  ZMQ, subprocesses, HTTP server, or Python. The server profile has all
  but Python, which arrives only as an installed kernelspec. Two seams to
  match rather than invent: `jupyter_server`'s gateway client for remote
  kernels, and `@jupyterlab/services`' `ServerConnection` for pointing a
  TS frontend at an in-app core, as JupyterLite does.

## Next

Resolve the open questions in `notes.md`, read cdp-kernel's generated
`kernel.json`, then start with a protocol crate and a CLI that runs one
cell on cdp-kernel.
