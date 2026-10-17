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
kernel, session, and contents flow. JupyterLab 4's own bundle is served
with `--static-dir` and runs notebooks once websockets authenticate (cookie
login is next); the VS Code Jupyter extension's remote-server flow passes.
See [`validation/`](validation/). The TypeScript frontend package has
the protocol plumbing and renderer seams with tests, but no app shell.

`ganymede-engine` is the library to embed and the `ganymede` CLI is built
on it. The e2e suite runs cdp-kernel and ipykernel through both and
compares against the reference: notebooks against `jupyter execute`
(nbclient), the HTTP and websocket API against `jupyter server`. The
engine builds for iOS, Android, and wasm, and runs a notebook inside wasm
under WASI.

## Code

Cargo workspace at this directory; `frontend/` is an npm package.

| Crate | What it is | Tests |
|---|---|---|
| [`crates/ganymede-protocol`](crates/ganymede-protocol/) | Message envelope, typed content with unknown-field passthrough, ZMQ wire framing with HMAC, connection file, websocket v1 and legacy codecs. No I/O. | 12 |
| [`crates/ganymede-nbformat`](crates/ganymede-nbformat/) | nbformat 4.5 reader and a writer byte-identical to Python `nbformat.write`. | 24 |
| [`crates/ganymede-kernel-host`](crates/ganymede-kernel-host/) | Kernelspec discovery, transport seam (ZMQ and in-process), subprocess and attach provisioners, the session state machine, a fake kernel. | 10 |
| [`crates/ganymede-exec`](crates/ganymede-exec/) | Notebook executor with nbclient's semantics: outputs, `clear_output(wait)`, `display_id` across cells, `language_info`, timings, widget state, allowed errors and tags, timeouts. | 4 |
| [`crates/ganymede-engine`](crates/ganymede-engine/) | The library to embed. `Engine` finds kernelspecs and starts, attaches, or adopts kernels; `Kernel` executes with streaming output and stdin, completes, inspects, interrupts, restarts, runs notebooks. Features `zmq` and `subprocess` (default) drop out for wasm. | 3 |
| [`crates/ganymede-cli`](crates/ganymede-cli/) | The `ganymede` command: `kernelspec list/paths/install/remove`, `run`, `console`, `execute`, `kernel`, `nb clear`, `serve`, `info`. | 26 |
| [`crates/ganymede-contents`](crates/ganymede-contents/) | Contents seam and filesystem implementation matching `jupyter_server`'s model: atomic writes, mtime conflicts, checkpoints, configurable checkpoint dir, path safety. | 30 |
| [`crates/ganymede-api`](crates/ganymede-api/) | Kernel and session managers, response models, per-client bridge with reply filtering, idle culling. No HTTP. | 6 |
| [`crates/ganymede-server`](crates/ganymede-server/) | axum routes, token auth, origin check, websocket v1 and legacy framing, static assets, TOML config, JupyterLab's page and `/lab/api/*`. | 13 |
| [`frontend/`](frontend/) | Protocol types, websocket codecs, `ServerConnection` with injectable `fetch` and `WebSocket`, REST client, `KernelConnection`, renderer registry, ANSI and stream handling, widget manager interfaces. | 45 |

Try it:

```
cargo install --path crates/ganymede-cli         # the `ganymede` binary
ganymede kernelspec list
ganymede run -k cdp-node 'const x = 2; x * 21'   # any installed kernelspec
ganymede run -k python3 -f script.py --json      # one JSON line per cell
ganymede console -k cdp-node                     # completion, multiline, Ctrl-C interrupts
ganymede execute notebook.ipynb -o out.ipynb     # like `jupyter execute`; --inplace, -o -
ganymede kernel -k python3                       # standalone kernel; prints its connection file
ganymede run --existing kernel-....json 'x'      # reuse it
ganymede serve --root-dir . --port 8888          # point VS Code or JupyterLab at the URL
ganymede serve --static-dir <prefix>/share/jupyter/lab/static   # JupyterLab at /lab
cd frontend && npm install && npm test
```

Embedding:

```rust
let engine = ganymede_engine::Engine::new();
let kernel = engine.start("cdp-node").await?;
let result = kernel.execute("1 + 1").await?;
println!("{}", result.text());
```

## Testing

`cargo test --workspace` runs everything with the in-process fake kernel.
The e2e tests in `crates/ganymede-cli/tests/` also use whatever real
kernels and Jupyter are installed, and skip what is missing:

| Variable | Effect |
|---|---|
| `GANYMEDE_E2E_KERNELS=cdp-node,python3` | These kernels must exist; their tests fail instead of skipping. |
| `GANYMEDE_E2E_JUPYTER=/path/to/jupyter` | The reference `jupyter` (needs `nbclient` and `jupyter_server`). Defaults to `PATH`. |
| `GANYMEDE_E2E_REQUIRE_JUPYTER=1` | Fail instead of skipping when it is missing. |
| `E2E_DUMP=1` | Print both server transcripts. |

- `kernels.rs`: the same scenarios on every kernel, through the engine and
  the CLI: kernel_info passthrough, results, streams, errors, stdin,
  interrupt, restart, comms, completion, inspection, standalone kernels.
- `parity.rs`: each notebook in [`e2e/notebooks/`](e2e/notebooks/) is
  executed by `jupyter execute` and by `ganymede execute`, and the files
  are compared after normalizing timestamps and ids. Mismatches are kept
  under `$TMPDIR/ganymede-parity-*`.
- `server.rs`: one REST and websocket scenario against `jupyter server`
  and `ganymede serve`, compared as transcripts.
- [`e2e/wasm-smoke/`](e2e/wasm-smoke/): the engine built for
  `wasm32-wasip1`, running a notebook on the fake kernel under Node's WASI.

CI runs the e2e job on Linux and macOS with Node 24, cdp-kernel 0.5.0,
and ipykernel, plus a portability job for iOS, Android, and wasm.

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

- Cookie login for browser frontends, which JupyterLab's websockets need.
- A trust store, so cells come back `trusted` as from `jupyter_server`.
- A runtime seam for the browser (timers and spawn on
  `wasm-bindgen-futures`) and a websocket or `postMessage` transport, so
  the engine runs in a page rather than only under WASI.
- Document model. A first UI of our own, outside the core: a browser app
  or a Swift shell with a web view.
- CurveZMQ for the ZMQ transport; ipykernel 7 already advertises it.
