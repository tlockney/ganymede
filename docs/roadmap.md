# Roadmap

What is done, and what comes next, in order. Each step lands as its own
pull request with a GitHub issue, and records what it learned in the
[development log](notes.md).

## Done

- **Core crates.** Protocol, kernel host, nbformat, executor, contents,
  API, and server, with the `ganymede` CLI and the embeddable
  `ganymede-engine`.
- **Parity with Jupyter.** An end-to-end suite runs cdp-kernel and
  ipykernel through Ganymede and compares the results with
  `jupyter execute` and `jupyter server`.
- **Existing frontends.** The VS Code Jupyter extension's remote-server
  flow passes. JupyterLab's own bundle is served at `/lab` with its
  `/lab/api/*` endpoints and runs notebooks in a browser, with cookie
  login as `jupyter_server` does it.
- **Portability.** The engine builds for iOS, Android, and wasm, and runs
  a notebook inside wasm under WASI.
- **Releases.** A `vX.Y.Z` tag builds and publishes binaries for Linux,
  macOS, and Windows.
- **Notebook trust.** Notebooks are signed on save and marked `trusted`
  on read as `jupyter_server` does, with Jupyter's key and signature
  database, and `ganymede trust` signs one by hand.

## Next

1. **cdp-kernel conformance** on Node 24: launch, execute, interrupt,
   restart, comms, and stdin, the same scenarios as ipykernel, with
   kernelspec metadata and `kernel_info_reply` extras passed through
   untouched.
2. **Kernel lifetime** for the iPad: kernels survive client disconnects,
   clients reattach by kernel id, and idle culling is configurable.
3. **Document model.** The file is the source of truth and is reloaded
   when it changes on disk (iCloud). A `yrs` CRDT layer is optional and
   later.
4. **A first UI of our own**, outside the core: a Swift shell with a web
   view for iPadOS and macOS, running a TypeScript notebook app, with the
   Rust core embedded in the same process.
5. **CurveZMQ** for the ZeroMQ transport. ipykernel 7 already advertises
   it.

## Open

- Restarting a kernel automatically when it crashes.
- A runtime seam for the browser (timers and `spawn` on
  `wasm-bindgen-futures`) and a websocket or `postMessage` transport, so
  the engine runs in a page and not only under WASI.
- Windows interrupts, which ipykernel does through a named event rather
  than a signal.

## Not planned

JupyterLab and `jupyter_server` extensions, nbconvert export, and the
JupyterHub single-user contract. See [Replacing Python](python-free.md)
for why.
