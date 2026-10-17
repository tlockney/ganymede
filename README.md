# Ganymede

A sketch of a Jupyter server in Rust with no Python anywhere in the
product: a kernel-agnostic kernel host, the messaging protocol, and the
server API. First target kernel is [cdp-kernel](https://cdp-kernel.org)
for TypeScript notebooks.

Named for Jupiter's largest moon, and for the cupbearer who served the
gods on Olympus. Started as `jupyter-core-rs` in
[tlockney/research](https://github.com/tlockney/research); the crates dropped
their old `jcore-` prefix for `ganymede-`.

Only the core server is Rust. The UI is pluggable and lives outside the
core: any Jupyter frontend, a TypeScript web UI, or a native shell such as
a Swift app on iPadOS with a web view for rendering. The `frontend/` package
is shared TypeScript for UIs that render in a web view, not part of the core.

- [`sketch.md`](sketch.md): components, what already exists in Rust, the
  parts that are usually missed, and a suggested order of work.
- [`notes.md`](notes.md): findings and open questions.

Section 6 of the sketch reads the server against the
[ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados) plan, where it is the remote tier.
Section 7 gives the module seams, the crate layout, and which modules
each deployment profile (server, desktop, iPad) uses.

## Status

Code sketch. The protocol, kernel host, nbformat, executor, contents, API,
and server crates build, pass their tests, and drive ipykernel over real
ZeroMQ. `@jupyterlab/services` connects to the server and runs the full
kernel, session, and contents flow. The TypeScript frontend package has
the protocol plumbing and renderer seams with tests, but no app shell.

## Code

Cargo workspace at this directory; `frontend/` is an npm package.

| Crate | What it is | Tests |
|---|---|---|
| [`crates/ganymede-protocol`](crates/ganymede-protocol/) | Message envelope, typed content with unknown-field passthrough, ZMQ wire framing with HMAC, connection file, websocket v1 and legacy codecs. No I/O. | 12 |
| [`crates/ganymede-nbformat`](crates/ganymede-nbformat/) | nbformat 4.5 reader and a writer byte-identical to Python `nbformat.write`. | 24 |
| [`crates/ganymede-kernel-host`](crates/ganymede-kernel-host/) | Kernelspec discovery, transport seam (ZMQ and in-process), subprocess and attach provisioners, the session state machine, a fake kernel. | 9 |
| [`crates/ganymede-exec`](crates/ganymede-exec/) | Headless executor: iopub messages to nbformat outputs, with `clear_output`, `display_id`, and stream merging. | 2 |
| [`crates/ganymede-cli`](crates/ganymede-cli/) | `ganymede kernelspec list`, `ganymede run`, `ganymede attach`, `ganymede exec`. | |
| [`crates/ganymede-contents`](crates/ganymede-contents/) | Contents seam and filesystem implementation matching `jupyter_server`'s model: atomic writes, mtime conflicts, checkpoints, configurable checkpoint dir, path safety. | 30 |
| [`crates/ganymede-api`](crates/ganymede-api/) | Kernel and session managers, response models, per-client bridge with reply filtering, idle culling. No HTTP. | 6 |
| [`crates/ganymede-server`](crates/ganymede-server/) | axum routes, token auth, origin check, websocket v1 and legacy framing, static assets, TOML config. | 5 |
| [`frontend/`](frontend/) | Protocol types, websocket codecs, `ServerConnection` with injectable `fetch` and `WebSocket`, REST client, `KernelConnection`, renderer registry, ANSI and stream handling, widget manager interfaces. | 45 |

Try it:

```
cargo test --workspace
cargo run -p ganymede-cli -- run --kernel fake "hello"
cargo run -p ganymede-cli -- kernelspec list
cargo run -p ganymede-cli -- run --kernel python3 "print(1+1)"   # any installed kernelspec
cargo run -p ganymede-cli -- exec notebook.ipynb -o out.ipynb
cargo run -p ganymede-server -- --root-dir . --port 8888      # then point JupyterLab or VS Code at the printed URL
cd frontend && npm install && npm test
```

## Findings

- The server half (protocol, kernel lifecycle, nbformat, REST and
  websocket server, CRDT document) is well-specified and suits Rust.
- The UI is not Rust and not part of the core. Editor, math, output
  renderers, and widgets are JavaScript, so any UI that shows rich output
  renders it in a web view, whether the shell around it is a browser or a
  Swift app.
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
  web view at an in-app core, as JupyterLite does.

## Next

- Point a real JupyterLab (static bundle via `--static-dir`, needs the
  `/lab/api/*` endpoints) and VS Code at the server.
- Cookie login for browser frontends; CurveZMQ.
- Run the conformance checks against cdp-kernel (needs Node 24).
- Document model. A first UI of our own, outside the core: a browser app
  or a Swift shell with a web view.
- CurveZMQ for the ZMQ transport; ipykernel 7 already advertises it.
