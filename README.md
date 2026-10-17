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

Code sketch. The protocol, kernel host, nbformat, and headless executor
crates build, pass their tests, and drive ipykernel over real ZeroMQ. The
TypeScript frontend package has the protocol plumbing and renderer seams
with tests, but no app shell. No server crate yet.

## Code

Cargo workspace at this directory; `frontend/` is an npm package.

| Crate | What it is | Tests |
|---|---|---|
| [`crates/jcore-protocol`](crates/jcore-protocol/) | Message envelope, typed content with unknown-field passthrough, ZMQ wire framing with HMAC, connection file, websocket v1 and legacy codecs. No I/O. | 12 |
| [`crates/jcore-nbformat`](crates/jcore-nbformat/) | nbformat 4.5 reader and a writer byte-identical to Python `nbformat.write`. | 24 |
| [`crates/jcore-kernel-host`](crates/jcore-kernel-host/) | Kernelspec discovery, transport seam (ZMQ and in-process), subprocess and attach provisioners, the session state machine, a fake kernel. | 9 |
| [`crates/jcore-exec`](crates/jcore-exec/) | Headless executor: iopub messages to nbformat outputs, with `clear_output`, `display_id`, and stream merging. | 2 |
| [`crates/jcore-cli`](crates/jcore-cli/) | `jcore kernelspec list`, `jcore run`, `jcore attach`, `jcore exec`. | |
| [`frontend/`](frontend/) | Protocol types, websocket codecs, `ServerConnection` with injectable `fetch` and `WebSocket`, REST client, `KernelConnection`, renderer registry, ANSI and stream handling, widget manager interfaces. | 45 |

Try it:

```
cargo test --workspace
cargo run -p jcore-cli -- run --kernel fake "hello"
cargo run -p jcore-cli -- kernelspec list
cargo run -p jcore-cli -- run --kernel python3 "print(1+1)"   # any installed kernelspec
cargo run -p jcore-cli -- exec notebook.ipynb -o out.ipynb
cd frontend && npm install && npm test
```

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

- Server crate: REST API and the websocket bridge over `KernelSession`,
  token auth, static assets. Validate with VS Code and JupyterLab.
- Run the conformance checks against cdp-kernel (needs Node 24).
- Contents crate, document model, and an app shell for the frontend.
- CurveZMQ for the ZMQ transport; ipykernel 7 already advertises it.
