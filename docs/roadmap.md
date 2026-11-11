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
- **Kernel conformance.** cdp-kernel (Node 24) and ipykernel run the same
  scenarios in CI: launch, execute, interrupt, restart, comms with binary
  buffers, stdin, aborted queues, history, display updates, the runtime
  exiting mid-cell, parallel starts, the debugger over control, and a
  malformed stdin frame, with kernelspec metadata and `kernel_info_reply`
  extras passed through untouched. Through `serve`, parallel starts, a crash,
  and a restart that fails are run on real kernels, and a weekly job runs the
  scenarios against cdp-kernel's `main`.
- **Kernel lifetime.** Kernels survive websocket drops, and a client that
  reconnects with the same `session_id` gets the output it missed, as
  `jupyter_server` buffers it. Idle culling is configurable. A kernel that
  crashes is restarted under the same id, as `jupyter_server`'s restarter
  does, and reported `dead` and removed after five crashes in a row; an
  attached one is watched by heartbeat. A start or restart whose kernel dies or doesn't
  answer is relaunched on fresh ports, up to three tries. Shutdown sends
  `shutdown_request`, then SIGTERM, then SIGKILL, and kernels die with the
  host.
- **JupyterLab without Python.** `ganymede lab install` fetches
  JupyterLab's prebuilt bundle from PyPI, `ganymede lab extension install`
  the prebuilt extensions (the widget manager and [anywidget](https://github.com/manzt/anywidget) by default),
  and `ganymede lab` serves and opens it.
- **Releases and install.** A `vX.Y.Z` tag builds and publishes binaries
  for Linux, macOS, and Windows (the Windows build is not tested beyond
  compiling). `install.sh` installs a release and cdp-kernel. CI's
  `workstation` job runs the documented install on a machine with no
  Python and drives JupyterLab with Playwright.
- **Notebook trust.** Notebooks are signed on save and marked `trusted`
  on read as `jupyter_server` does, with Jupyter's key and signature
  database, and `ganymede trust` signs one by hand.
- **A UI of our own.** [ganymede-ui](https://github.com/tlockney/ganymede-ui)
  is a notebook app built on JupyterLab's packages, outside the core. It
  runs in a browser against Ganymede or with kernels in the page, and in
  a Mac, iPad and iPhone app that runs the core in-process through
  `ganymede-ffi`.
- **Remote kernels.** `--gateway-url` runs kernels on another Jupyter or
  Ganymede server while files stay local, which is how the iPad app gets
  kernels it can't launch.
- **nbformat 3.** Older notebooks are upgraded to 4.5 on read.
- **Terminals and language servers.** `/api/terminals` over a
  pseudo-terminal, and `jupyter-lsp`'s relay to language servers from the
  config file, both with no Python
  ([JupyterLab and VS Code](frontends.md#terminals)).
- **Document rooms.** The file is the source of truth: a room holds the
  live notebook as a `yrs` document, reloads it when the file changes on
  disk, merges outside changes cell by cell, and runs cells into it
  through the execute endpoint
  ([Live editing and document rooms](rooms.md)).
- **Agents.** `ganymede mcp` gives coding agents notebook tools, and the
  ganymede-journal plugin for Claude Code keeps an analysis as a notebook
  while the agent works ([Agents over MCP](mcp.md)).
- **Observability.** A kernel's log (`/api/kernels/{id}/log`),
  `jupyter-resource-usage`'s memory and CPU metrics, and opt-in crash
  reports with no notebook content ([Privacy](privacy.md)).
- **Signed apps.** The Mac app is signed and notarized in each ganymede-ui
  release, and the iPad and iPhone app is on TestFlight for internal
  testers.
- **Parameters on app links.** `ganymede://run` links and Shortcuts pass
  parameters to a run (ganymede-ui 0.7.0)
  ([ganymede-ui#303](https://github.com/tlockney/ganymede-ui/issues/303)).
- **Parameters and named results.** `ganymede execute -p` and
  `--params-file` pass parameters, `--results` prints the named results,
  and `POST /api/notebooks/run` does the same over HTTP
  ([Parameters and named results](reference.md#parameters-and-named-results)).
- **Host API.** Cells read, search and run notebooks, and edit their
  own, through the host comm: `notebooks.list`, `read`, `search`,
  `results` and `run`, and `self.*`, under per-notebook grants set with
  `ganymede grant`, the grants route, or the apps' Notebook Access sheet
  ([A host API for kernels](host-api.md)).
- **In-app kernels as server kernels.** Ganymede's C functions let an
  app register its own kernels with the embedded server
  ([In-app kernels as server kernels](in-app-kernel.md)).

### Done on `main`

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

- **Run copies.** `save: "copy"` on a run keeps the executed notebook
  under `runs/` ([Run copies](reference.md#run-copies)).

## Next

In order of priority.

1. **The notebook as a program.** A notebook takes parameters, returns
   named results, and runs from the CLI, the server API, app links,
   Shortcuts and other notebooks, through one run contract. Grants are
   per notebook and only narrow. Next:
   - granting from `ganymede execute`
     ([#647](https://github.com/tlockney/ganymede/issues/647)).
2. **The apps as a full runtime.** The apps run notebooks in-process,
   with no external server. Next:
   - a native in-app engine, jitless V8, unless its size or speed on a
     device rules it out (measured in `spikes/v8-engine/`);
   - device APIs ([ganymede-device](https://github.com/tlockney/ganymede-device));
   - automation: Shortcuts, links, MCP from the Mac app, Spotlight
     ([ganymede-ui#301](https://github.com/tlockney/ganymede-ui/issues/301));
   - widgets ([ganymede-ui#436](https://github.com/tlockney/ganymede-ui/issues/436))
     and the share extension
     ([ganymede-ui#449](https://github.com/tlockney/ganymede-ui/issues/449)).

     > [!NOTE]
     > **Unreleased in ganymede-ui.** The share extension is on ganymede-ui's
     > `main`, off by default behind a Settings switch
     > ([ganymede-ui#477](https://github.com/tlockney/ganymede-ui/pull/477)),
     > not in its latest release.
3. **A person and an agent in one live notebook.** The app opens
   notebooks through rooms, so edits from an agent, another window or
   another device arrive live, and shows the agent's journal as it is
   written ([ganymede-ui#183](https://github.com/tlockney/ganymede-ui/issues/183),
   [#184](https://github.com/tlockney/ganymede-ui/issues/184)). Journal
   sessions get evals
   ([#316](https://github.com/tlockney/ganymede/issues/316)).
4. **TypeScript notebooks for TypeScript developers.**
   [cdp-kernel](https://github.com/tlockney/cdp-kernel) is the
   first-class kernel, on Node and Deno first.

## Not now

Wanted eventually, but not being worked on:

- A Python kernel in the apps through Pyodide
  ([ganymede-ui#258](https://github.com/tlockney/ganymede-ui/issues/258)).
  Python would stay out of the product: a kernel the page loads.
- MCP Apps: a read-only notebook view for chat hosts
  ([#643](https://github.com/tlockney/ganymede/issues/643)).
- A native kernel for a language other than JavaScript.
- CurveZMQ for the ZeroMQ transport. ipykernel 7 already advertises it.

## Open

- Trusting `tailscale serve`'s identity headers, so a client on the
  tailnet needs no token.
- A Linux build for musl (Alpine).
- Distributing the iPad and iPhone app beyond internal TestFlight
  testers, which needs App Review (rule 2.5.2, apps that run code). The
  Mac app is released, signed and notarized.
- A runtime seam for the browser (timers and `spawn` on
  `wasm-bindgen-futures`) and a websocket or `postMessage` transport, so
  the engine runs in a page and not only under WASI.
- Windows interrupts, which ipykernel does through a named event rather
  than a signal.

## Not planned

JupyterLab server extensions and `jupyter_server` extensions (prebuilt
JupyterLab frontend extensions do work), nbconvert's template exports
(HTML, PDF, LaTeX, slides), and the
JupyterHub single-user contract. See [Replacing Python](python-free.md)
for why.
