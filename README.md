# Ganymede

A Jupyter server in Rust, with no Python anywhere in the product.
Ganymede launches kernels from installed kernelspecs, speaks the Jupyter
messaging protocol to them, reads and writes notebooks, and serves the
same REST and websocket API as `jupyter_server`, so JupyterLab and VS Code
connect to it unchanged. The first target kernel is
[cdp-kernel](https://cdp-kernel.org), for TypeScript notebooks; any other
kernel, Python's included, works once its kernelspec is installed.

Named for Jupiter's largest moon, and for the cupbearer who served the
gods on Olympus.

**Documentation:** [tlockney.github.io/ganymede](https://tlockney.github.io/ganymede/)
(sources in [`docs/`](docs/)). Start with
[Getting started](docs/getting-started.md) and
[Architecture](docs/architecture.md).

## Install

Prebuilt binaries for Linux (x86_64, aarch64), macOS (Apple silicon,
Intel), and Windows (x86_64) are on the
[releases page](https://github.com/tlockney/ganymede/releases). On Linux or
macOS, this script downloads the latest release, checks its SHA-256,
installs `ganymede` into `~/.local/bin`, and, if Node 24 or later is
installed, installs [cdp-kernel](https://cdp-kernel.org) for TypeScript
notebooks:

```sh
curl -fsSL https://tlockney.github.io/ganymede/install.sh | sh
```

Pass options after `sh -s --`: `--version 0.1.0` pins a release,
`--bin-dir DIR` installs elsewhere, `--no-kernel` skips cdp-kernel.
[Install](docs/install.md) has the same steps done by hand, and the
platform notes (glibc 2.35 or newer on Linux; unnotarized macOS binaries).

Quick test, no kernel or Python needed (`fake` is a built-in kernel):

```sh
ganymede --version                 # ganymede 0.1.0
ganymede info                      # Jupyter paths in use
ganymede run -k fake 'hello'       # prints "echo: hello" and "HELLO"
ganymede serve --port 8888 --token test &
curl -H 'Authorization: token test' http://127.0.0.1:8888/api/status
kill %1
```

With a real kernel installed, for example cdp-kernel on Node or ipykernel:

```sh
npm install -g @tlockney/cdp-kernel && cdp-kernel install --runtime node
ganymede kernelspec list
ganymede run -k cdp-node 'const x = 2; x * 21'
```

Or build from source with `cargo install --path crates/ganymede-cli`.
[Install](docs/install.md) has more on both.

## Use

```
ganymede kernelspec list
ganymede run -k cdp-node 'const x = 2; x * 21'   # any installed kernelspec
ganymede run -k python3 -f script.py --json      # one JSON line per cell
ganymede console -k cdp-node                     # completion, multiline, Ctrl-C interrupts
ganymede execute notebook.ipynb -o out.ipynb     # like `jupyter execute`; --inplace, -o -
ganymede kernel -k python3                       # standalone kernel; prints its connection file
ganymede run --existing kernel-....json 'x'      # reuse it
ganymede serve --root-dir . --port 8888          # point VS Code or JupyterLab at the URL
ganymede lab install                             # JupyterLab's frontend, from PyPI, no Python
ganymede serve                                   # ... then served at /lab
ganymede serve --static-dir <prefix>/share/jupyter/lab/static   # or one installed elsewhere
```

`-k fake` is a built-in in-process kernel for trying things out without
installing one.

To embed it, use `ganymede-engine`:

```rust
let engine = ganymede_engine::Engine::new();
let kernel = engine.start("cdp-node").await?;
let result = kernel.execute("1 + 1").await?;
println!("{}", result.text());
```

## Status

Early software. The protocol, kernel host, nbformat, executor, contents,
API, and server pass their tests, and an end-to-end suite compares
Ganymede against `jupyter execute` and `jupyter server` on cdp-kernel and
ipykernel. The VS Code Jupyter extension's remote-server flow passes.
JupyterLab's own bundle is served at `/lab` and runs notebooks once its
websockets authenticate, which needs cookie login, the next step. The
engine builds for iOS, Android, and wasm. See the
[roadmap](docs/roadmap.md).

Only the core is Rust. UIs are clients of its API and live outside it:
existing Jupyter frontends, a TypeScript web UI, or a native shell such as
a Swift app with a web view. JupyterLab and `jupyter_server` extensions
are not supported, by design.

## Repository

Cargo workspace at this directory. `frontend/` is shared TypeScript for
UIs that render in a web view, `docs/` and `site/` are the website, and
`validation/` holds scripts that drive real JupyterLab and VS Code clients.

| Crate | What it is | Tests |
|---|---|---|
| [`crates/ganymede-protocol`](crates/ganymede-protocol/) | Message envelope, typed content with unknown-field passthrough, ZMQ wire framing with HMAC, connection file, websocket v1 and legacy codecs. No I/O. | 12 |
| [`crates/ganymede-nbformat`](crates/ganymede-nbformat/) | nbformat 4.5 reader and a writer byte-identical to Python `nbformat.write`. | 24 |
| [`crates/ganymede-kernel-host`](crates/ganymede-kernel-host/) | Kernelspec discovery, transport seam (ZMQ and in-process), subprocess and attach provisioners, the session state machine, a fake kernel. | 10 |
| [`crates/ganymede-exec`](crates/ganymede-exec/) | Notebook executor with nbclient's semantics: outputs, `clear_output(wait)`, `display_id` across cells, `language_info`, timings, widget state, allowed errors and tags, timeouts. | 4 |
| [`crates/ganymede-engine`](crates/ganymede-engine/) | The library to embed. `Engine` finds kernelspecs and starts, attaches, or adopts kernels; `Kernel` executes with streaming output and stdin, completes, inspects, interrupts, restarts, runs notebooks. Features `zmq` and `subprocess` (default) drop out for wasm. | 3 |
| [`crates/ganymede-cli`](crates/ganymede-cli/) | The `ganymede` command: `kernelspec list/paths/install/remove`, `run`, `console`, `execute`, `kernel`, `nb clear`, `lab install/list/remove`, `serve`, `info`. | 31 |
| [`crates/ganymede-contents`](crates/ganymede-contents/) | Contents seam and filesystem implementation matching `jupyter_server`'s model: atomic writes, mtime conflicts, checkpoints, configurable checkpoint dir, path safety. | 30 |
| [`crates/ganymede-api`](crates/ganymede-api/) | Kernel and session managers, response models, per-client bridge with reply filtering, idle culling. No HTTP. | 6 |
| [`crates/ganymede-server`](crates/ganymede-server/) | axum routes, token auth, origin check, websocket v1 and legacy framing, static assets, TOML config, JupyterLab's page and `/lab/api/*`, installed JupyterLab bundles. | 15 |
| [`frontend/`](frontend/) | Protocol types, websocket codecs, `ServerConnection` with injectable `fetch` and `WebSocket`, REST client, `KernelConnection`, renderer registry, ANSI and stream handling, widget manager interfaces. | 45 |

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
| `GANYMEDE_E2E_LAB=1` | Run `lab.rs`, which downloads JupyterLab from PyPI (about 17 MB). |

- `kernels.rs`: the same scenarios on every kernel, through the engine and
  the CLI: kernel_info passthrough, results, streams, errors, stdin,
  interrupt, restart, comms, completion, inspection, standalone kernels.
- `parity.rs`: each notebook in [`e2e/notebooks/`](e2e/notebooks/) is
  executed by `jupyter execute` and by `ganymede execute`, and the files
  are compared after normalizing timestamps and ids. Mismatches are kept
  under `$TMPDIR/ganymede-parity-*`.
- `server.rs`: one REST and websocket scenario against `jupyter server`
  and `ganymede serve`, compared as transcripts.
- `lab.rs`: `ganymede lab install` from PyPI, then `ganymede serve`
  answering `/lab` and `/lab/api/settings` from the installed bundle.
- [`e2e/wasm-smoke/`](e2e/wasm-smoke/): the engine built for
  `wasm32-wasip1`, running a notebook on the fake kernel under Node's WASI.

CI runs the e2e job on Linux and macOS with Node 24, cdp-kernel 0.5.0,
and ipykernel, plus a portability job for iOS, Android, and wasm.

## Contributing

Work is tracked in [GitHub issues](https://github.com/tlockney/ganymede/issues):
each task and each bug found gets an issue before work starts, and every
pull request links the issue it resolves (`Closes #N`), which CI checks,
and appends what it learned to the [development log](docs/notes.md). The
[Development](docs/development.md) page has the rules and how to build the
docs.

Releases are cut from an up-to-date `main` with
[`scripts/release.sh`](scripts/release.sh), which wraps
[cargo-release](https://github.com/crate-ci/cargo-release) and
[git-cliff](https://git-cliff.org)
(`cargo install --locked cargo-release git-cliff`):

```sh
scripts/release.sh minor            # dry run: version bump and changelog preview
scripts/release.sh minor --execute  # bump, CHANGELOG.md, commit, tag vX.Y.Z, push
```

The level is `patch`, `minor`, `major`, or an explicit version such as
`0.2.0-rc.1`. All crates share the workspace version, so the tag, the
crates, and `ganymede --version` agree. [`CHANGELOG.md`](CHANGELOG.md)
lists the pull requests merged into each release, from the merge commits.
The tag runs [`release.yml`](.github/workflows/release.yml), which builds
the five targets above, checks each native binary reports the tagged
version, and publishes a GitHub Release with `ganymede-X.Y.Z-<target>.tar.gz`
(`.zip` on Windows), a `.sha256` per archive, `SHA256SUMS`, and that
version's changelog section as notes. Versions with a `-` suffix are
marked prerelease.

## License

MIT.
