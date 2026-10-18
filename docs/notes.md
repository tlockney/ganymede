# Development log

Findings from each step, newest last: what was built, what was checked
against the reference implementations, what broke, and what was learned.
Each change appends an entry here.

The design started as a single `sketch.md`, split on 2026-10-17 into
[Architecture](architecture.md), [Replacing Python](python-free.md),
[Protocol and kernel host](protocol.md), [User interfaces](ui.md),
[The iPad use case](ipados.md), [Roadmap](roadmap.md), and
[Prior art](prior-art.md). Older entries refer to the sketch by section
number, and entries before the rename use the old `jcore-` crate names.

## 2026-10-17

Starting point: question of what a Rust reimplementation of the Jupyter
core (kernel host, web UI, protocol) would take and what is easy to miss.

Wrote `sketch.md`: components, what already exists in Rust,
commonly missed pieces, and an order of work.

Findings while writing it:

- Messaging spec is now v5.5: `subshell_id` in headers, iopub moved to
  XPUB with an `iopub_welcome` message, `supported_features` in
  `kernel_info_reply` (JEP 92).
- The runtimed crates (`jupyter-protocol`, `runtimelib`, `nbformat`) are
  maintained and used by Zed's REPL. They cover protocol, ZMQ kernel
  client, and the file format. A new project should either build on them
  or use them as a conformance oracle.
- No Rust equivalent of `jupyter_server` exists. That plus the frontend
  is the actual work.
- The frontend cannot usefully be Rust-native if `text/html` outputs and
  widgets matter. TypeScript UI over a Rust server is the pragmatic shape.
  (Superseded by the scope clarification at the end of these notes: the
  UI was never meant to be Rust, and is pluggable.)

Open questions:

- Is the goal a product, or understanding the system? Changes whether to
  reuse the runtimed crates.
- Are widgets in scope? Largest single cost after the UI itself.
- Windows support?
- Must existing clients (JupyterLab, VS Code, papermill) connect to it?
  If yes, the REST and websocket surface is fixed by `jupyter_server`.

Nothing built.

## 2026-10-17, later

Goal clarified: a Jupyter server with no Python anywhere in the product,
kernel-agnostic, with [cdp-kernel](https://cdp-kernel.org) (TypeScript on
Node or Deno) as the first target. May become a project of its own.

Read cdp-kernel's install, architecture, and known-gaps pages. Relevant
facts:

- Speaks ZMTP over TCP via `node:net`; no `ipc` transport.
- Installs kernelspecs to the standard Jupyter data dirs
  (`$JUPYTER_DATA_DIR`, `~/Library/Jupyter`, XDG). Uninstall docs still
  point at the Python `jupyter kernelspec` CLI, so the server should ship
  `kernelspec list/install/remove`.
- Interrupts over CDP internally; `kernel.json` contents (including
  `interrupt_mode`) are not documented. Read the generated file.
- anywidget-style widgets and comms only; no ipywidgets controls.
- Debugger on Node and Deno targets; needs a DAP client in the UI.
- `prompt()` and `confirm()` over stdin.
- Kernel inherits the server's environment. Windows unsupported.

Changes to `sketch.md`: revised goal paragraph, new section 3 (table of
Python assumptions in the reference stack and their replacements, what
cdp-kernel needs from a server, frontends that work today), order of work
now validates the server with VS Code plus cdp-kernel before any UI, and
more prior art (Jupyverse, VS Code Jupyter extension, anywidget,
Datalayer jupyter-react).

Main conclusion: step 4 (the server) is a usable product on its own.
VS Code's Jupyter extension against a Rust server running cdp-kernel is
a fully Python-free TypeScript notebook environment with no UI work.

Open questions, updated:

- Build on the runtimed crates or reimplement? Still open.
- Which `interrupt_mode` does cdp-kernel declare?
- Serve JupyterLab's static bundle (needs `/lab/api/*`) or target VS Code
  only for v1?
- Widgets: anywidget-only manager in a custom UI, or rely on JupyterLab
  and VS Code's managers until a UI exists?

## 2026-10-17, ts-on-ipados as a use case

The [ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados) plan is one intended use: this
server is its remote tier on the Mac mini over the tailnet. Read the
design note sections 04, 05, 09 and the review sections 3, 4, 6, 7.
Added section 6 to `sketch.md`.

What the plan adds to the server's requirements:

- REST API fidelity, because Juno Connect and JupyterLab in Safari are
  the Tier 0 clients.
- Token auth baseline; `tailscale serve` identity headers as an option.
- Kernels survive websocket drops (iPad suspension) and clients reattach
  by id; idle culling as the complement.
- Files are truth: iCloud Drive writes under the server. Needs mtime
  conflict check on PUT, atomic writes, checkpoint dir outside the synced
  folder.
- The websocket and channel-message shape become a public contract once
  the Swift client exists.
- Library-first workspace so the iPad app can compile the protocol,
  nbformat, and document crates for iOS and drive its in-process kernel
  through the same types. ZMQ is one transport behind a trait.
- An attach-to-existing-kernel provisioner, for driving the device kernel
  from a desktop JupyterLab during development.
- Pass kernelspec `metadata` and `kernel_info_reply` through verbatim so
  capability manifests reach the client.

Tension noted: a server-side CRDT and iCloud file sync are two writers
of one file. For this use case, file-as-truth with the mtime check is
enough for v1.

## 2026-10-17, modules and profiles

Goal restated: pluggable modules composing a Jupyter-like environment on
iPad, desktop, or server; TypeScript kernels as the focus; Python kernels
possible on desktop and server but never assumed. Added section 7 to
`sketch.md`: the one rule (the host knows no language), the seams with
their implementations, the three profiles, a crate layout, and a ranked
definition of ecosystem compatibility.

Decisions implied:

- Kernel host boundary is "a stream of channel messages", with ZMQ,
  in-process, and websocket-client transports behind one trait.
- The REST and websocket handlers are functions over traits, not axum
  handlers, so the iPad bridge and the HTTP server share them.
- The widget manager is per profile: anywidget-only on iPad, the full
  ipywidgets manager where Python kernels are expected.
- Extension compatibility (JupyterLab, jupyter_server) is out, by design.

Found two existing seams to match: `jupyter_server`'s gateway client
(remote provisioner) and `@jupyterlab/services` `ServerConnection` with
custom `fetch` and `WebSocket` (how JupyterLite runs without a server).
JupyterLite is the closest precedent for the iPad profile.

Test rule to adopt: the same scenarios run against cdp-kernel and
ipykernel; no `python` string in host crates outside tests.

## 2026-10-17, code sketch

Started the code. Cargo workspace at the project root, `crates/*`, and a
TypeScript package under `frontend/`. Everything builds, tests pass,
clippy is clean. Test oracle: a scratch venv with ipykernel 7 and
nbformat 5.11 (cdp-kernel needs Node 24; this container has 22).

Crates:

- `jcore-protocol`: envelope, typed content with `extra` passthrough,
  ZMQ wire framing with HMAC-SHA256, connection file, websocket v1 and
  legacy codecs. No I/O. 12 tests, including an HMAC vector checked
  against Python and the v1 offset layout from `jupyter_server`.
- `jcore-nbformat` (written by a subagent): nbformat 4.5 types, lenient
  reader, byte-exact writer matching `nbformat.write`. 24 tests against
  fixtures generated by Python nbformat.
- `jcore-kernel-host`: kernelspec discovery over the Jupyter data dirs,
  the transport seam with an in-process pair, the ZMQ transport on the
  pure-Rust `zeromq` crate, subprocess and attach provisioners, the
  session state machine, and a fake kernel. 9 tests.
- `jcore-exec`: headless executor over the two above. 2 tests.
- `jcore-cli`: `jcore kernelspec list|paths`, `jcore run`, `jcore attach`,
  `jcore exec`.

Verified against ipykernel over real ZMQ:

- `jcore run --kernel python3 ...`: launch, nudge, five cells, shutdown,
  about 1.6 s total.
- `--interrupt-after-ms`: SIGINT to the process group interrupts a
  sleeping cell; `interrupt_request` on control does the same on the fake.
- `jcore attach <connection file>` against a kernel started by
  `jupyter_client`.
- `jcore exec`: a notebook generated by nbformat with streams, a
  `display_id` update, `clear_output(wait=True)`, and an empty cell. The
  output validates with nbformat and is byte-identical after an nbformat
  rewrite.

Things learned:

- `tokio::sync::watch::Sender::send` fails when no receiver exists; use
  `send_replace` for state that may have no watchers yet.
- ipykernel 7 reports protocol 5.3 in `kernel_info_reply` while its
  kernelspec says 5.5. Do not gate on the version string.
- ipykernel warns about unencrypted TCP and advertises
  `supported_encryption: ["curve"]` in kernelspec metadata. CurveZMQ is a
  future item for the ZMQ transport.
- Python `json.dumps` float layout (`1e-05`, `2.5e-07`) differs from
  serde_json; the nbformat writer has its own float formatter.
- `nbformat.write` assigns ids to cells that lack them, so byte fidelity
  is only defined for notebooks with ids.

Frontend (written by a subagent): protocol types, the websocket v1 and
legacy codecs, a `ServerConnection` with injectable `fetch` and
`WebSocket`, a REST client, a `KernelConnection` with the two-part
completion rule, a renderer registry, ANSI and stream handling, and
widget manager interfaces. 45 tests on Node's test runner, `typescript`
as the only dependency. No editor, no DOM tests, no app shell yet.

Not done, in the order the sketch suggests: the server crate (REST and
websocket bridge over the session), the contents crate, the document
model, an app shell for the frontend, cdp-kernel verification (needs
Node 24), CurveZMQ, Windows.

## 2026-10-17, server crate

Added three crates. `jcore-contents` (subagent): the contents seam and a
filesystem implementation with atomic writes, the mtime conflict check,
checkpoints with a configurable directory outside the root, and path
safety including symlink escapes; 30 tests. `jcore-api`: kernel and
session managers, models, and the per-client bridge, with no HTTP; 6
tests. `jcore-server`: axum routes, token auth, websocket channels, TOML
config, static assets; 5 integration tests including a real websocket
round trip against the fake kernel.

Verified against ipykernel over ZMQ, through HTTP, from Node:

- Our `frontend/` client: kernelspecs, start, kernel_info, execute with
  stdout and stderr, result, error reply, shutdown.
- `@jupyterlab/services` 7.5, the real JupyterLab client library: specs,
  start, info, execute, complete, signal interrupt of a sleeping cell,
  restart (it reconnects its websocket on its own), contents newUntitled,
  save, get, list, sessions bound to the kernel, shutdown, and the server's
  close frame when the kernel is gone.

Things learned:

- JupyterLab's client POSTs JSON without `Content-Type`; axum's `Json`
  extractor answers 415. `jupyter_server` parses the body regardless. The
  server now has a lenient extractor for every body route.
- A client holding an `Arc` to the running kernel keeps the broadcast hub
  alive, so "hub closed" can never signal shutdown; an explicit alive flag
  the client selects on does.
- `KernelSession::shutdown` must not wait on or kill a process it does
  not own; attached kernels have no pid.
- A kernel started with no path must run in the contents root, not the
  server's cwd.
- `pkill -f <pattern>` matches the shell running it. Use process names.
- The contents crate's checkpoint layout with an external directory
  mirrors the root's tree instead of jupyter_server's flat folder, so
  same-named files in different directories do not collide.

Not done: cookie login for a browser frontend, serving JupyterLab's static
bundle (needs `/lab/api/*`), CurveZMQ, kernel autorestart on crash,
cdp-kernel verification.

## Scope clarification: the UI is not Rust (2026-10-17)

Only the core server is Rust. The UI was never meant to be, and should
itself be pluggable: any Jupyter frontend, a TypeScript web UI, or a
native shell, for example a Swift app on iPadOS with a web view doing the
rendering. Earlier drafts read as if the presentation layer was part of
the Rust project (scope line, section 2.6 weighing Rust UI frameworks,
`frontend/` described as "the" frontend). The sketch, README, and section
7 now treat the UI as a client of the core's API, with the TS package as
shared rendering code for any web-view UI. No code changes: the Rust
crates never depended on a UI.


## 2026-10-17, rename to ganymede-

Renamed the crates from `jcore-*` to `ganymede-*`, the CLI binary from
`jcore` to `ganymede`, the server binary from `jcore-server` to
`ganymede-server`, and the npm package to `@ganymede/frontend`. Entries
above this one keep the old names.

Things learned:

- The longer crate prefix pushes some `use` lines and signatures past
  rustfmt's width, so a rename needs `cargo fmt` before CI's
  `fmt --check` passes.

## 2026-10-17, validation against JupyterLab and VS Code

Pointed JupyterLab 4.6.4 and the VS Code Jupyter extension's remote-server
flow at the server, with ipykernel 7 as the kernel (cdp-kernel needs
Node 24; this container has 22). For JupyterLab, a real `jupyter lab`
(jupyter_server 2.21.1) was the oracle: the same browser script ran
against both and the request sets were diffed. Scripts are in
[`validation/`](../validation/).

No real VS Code instance can run in this container. Instead,
`validation/vscode.cjs` replays the extension's flow from its source
(vscode-jupyter `e116e22`, `src/kernels/jupyter` and
`src/standalone/userJupyterServer`): the same URL parsing, password probe,
`@jupyterlab/services` settings, `Request` and `WebSocket` overrides, and
manager calls. It passes all 13 steps against both servers.

Added:

- JupyterLab mode. `--static-dir` pointed at JupyterLab's `static/`
  directory is recognised by its `index.html` and `package.json`; the
  server then serves the page at `/lab`, `/lab/tree/*`, and
  `/lab/workspaces/*` with a page config, the bundle at `/static/lab`,
  `schemas/` and `themes/` from the sibling directories, and prebuilt
  extensions found in `labextensions` under the Jupyter data dirs or
  `--labextensions-path`. `/` redirects to `/lab`.
- `/lab/api/settings` (JSON5 user settings under
  `<config dir>/lab/user-settings`), `/lab/api/workspaces`,
  `/lab/api/translations` (English only), `/lab/api/listings`,
  `/lab/api/plugins`, `/lab/api/licenses` (JSON format only),
  `/lab/api/themes`.
- Core `jupyter_server` endpoints JupyterLab calls: `GET /api` (public,
  reports the API version matched), `/api/me`, `/api/config/{section}`
  (GET, PATCH with recursive merge, stored in `<config dir>/nbconfig`),
  `/api/nbconvert` (empty: nbconvert is Python), `/api/events/subscribe`
  (accepted, silent), and kernelspec `resources` with
  `/kernelspecs/{name}/{file}`.
- `--config-dir` for that frontend state; default `JUPYTER_CONFIG_DIR` or
  `~/.jupyter`, the same place `jupyter_server` keeps it.

Endpoints JupyterLab called, opening a notebook, running a cell, saving:
`/lab/workspaces/{ws}/tree/{path}`, `/static/lab/*`,
`/lab/extensions/*`, `/lab/api/settings` (plain and `?ids_only=true`),
`/lab/api/settings/{id}`, `/lab/api/themes/{pkg}/index.css`,
`/lab/api/translations`, `/lab/api/translations/default`,
`/lab/api/workspaces` (GET, PUT every few seconds), `/api/me`,
`/api/config/jupyterlabapputilsextensionannouncements`, `/api/nbconvert`,
`/api/kernelspecs`, `/api/kernels`, `/api/sessions` (GET, POST, PATCH
twice), `/api/contents` (root listing, `content=0&hash=0` polls, the
notebook with `hash=1`, PUT), `/api/contents/{path}/checkpoints` (GET,
POST), websockets `/api/events/subscribe` and
`/api/kernels/{id}/channels?session_id=`, `/lsp/status`, and the
kernelspec logo. Only against jupyter_server, because of its page config:
`/api/terminals` and `/lab/api/build`. Every REST URL carries a
cache-busting `?<timestamp>` query.

VS Code's flow calls: `GET /` and `GET /tree?` without credentials (the
password probe; 404 on `/tree?` means "no password"), `/api/kernels`,
`/api/kernelspecs`, `/api/sessions` (GET, POST with a
`<name>-jvsc-<uuid>.ipynb` path that never exists, DELETE),
`/api/kernels/{id}/interrupt`, `/api/kernels/{id}/restart`, and the
channels websocket with `Authorization` as a handshake header and
`token` in the query. It never calls `/api`, `/api/me`, `/api/status`, or
the contents API. It forces `Content-Type: application/json` on every
request, GET and DELETE included, and pings the websocket every 30 s.

What broke:

- stdin never reached the client. A kernel sends `input_request` on its
  stdin ROUTER to the routing identity that sent the `execute_request` on
  shell. The host's shell and stdin DEALERs had separate random
  identities, so the kernel addressed a peer the stdin socket was not.
  Fixed: one identity for shell, control, and stdin, as `jupyter_client`
  does, with a test. Neither earlier client test exercised `input()`.
- JupyterLab's websockets get 403 without cookie login. In a browser at
  the server's own origin, `@jupyterlab/services` sets `appendToken` to
  false, so the events and channels websockets carry no token and rely on
  the login cookie `jupyter_server` sets when the page is loaded with
  `?token=`. REST calls are fine (the page config token goes in the
  `Authorization` header). The kernelspec logo `<img>` gets 403 for the
  same reason. With the token appended to websocket URLs by the script
  (`WS_TOKEN=1`), JupyterLab opens a notebook, starts ipykernel, runs a
  cell, and saves a notebook nbformat validates. This is step 3.
- `/lsp/status` is 404, as with any jupyter_server without the
  `jupyter-lsp` extension. JupyterLab logs it and carries on; the oracle
  had `jupyter-lsp` installed, which lists Python language servers.
- On restart, `@jupyterlab/services` reports `autorestarting` and
  reconnects twice; against jupyter_server it reports `restarting` and
  reconnects once. The restart still succeeds. Belongs with kernel
  lifetime (step 5).

Things learned:

- JupyterLab's `index.html` is a Jinja template. The server renders the
  few constructs it uses; `tojson` must escape `<`, `>`, `&`, `'` so the
  page config cannot close its `<script>`.
- `jupyterlab_server` returns settings `raw` as the user's JSON5 text and
  `settings` parsed. Users write comments and trailing commas, so a JSON
  parser is not enough.
- The page config's `extensionManager` set to `read-only` with
  `can_install: false` keeps the extension manager from offering PyPI
  installs. `buildAvailable` and `terminalsAvailable` false stop the
  `/lab/api/build` and `/api/terminals` calls.
- `jupyter_server` serves `/static/lab`, themes, and extension assets
  without auth; the page, `/lab/api/*`, and kernelspec resources need it;
  `GET /api` is public.
- Chromium does not apply Playwright's `extraHTTPHeaders` to websocket
  handshakes, so it cannot stand in for cookie auth.
- `@jupyterlab/services` 7.5 appends `Authorization: token` even when the
  request already has one, giving `token t, token t`. jupyter_server
  rejects that; VS Code's `Request` subclass blocks the second append.

Not done: Notebook 7's `/tree` and `/notebooks` pages, the terminal and
LSP endpoints, language packs, `overrides.json` and `page_config.json` from
the app's `settings/` dir, and a real VS Code instance.

## 2026-10-17, engine, CLI, and e2e parity with Jupyter

`ganymede-engine` is the one library a host embeds: `Engine` finds
kernelspecs and starts, attaches, connects, or adopts kernels; `Kernel` is
one running kernel (execute with a `Handler` for streamed iopub and stdin,
complete, inspect, is_complete, comms, interrupt, restart, shutdown, run a
notebook). The `ganymede` CLI is a thin layer over it, and `ganymede serve`
replaces the separate server binary as the usual entry point.

`ganymede-exec` now follows nbclient where its behavior is observable in
the saved file, so `ganymede execute` and `jupyter execute` write the
same notebook. Checked on ipykernel 7.4 and cdp-kernel 0.5 with
nbclient 0.11 and jupyter_server 2.21:

- Python notebook: byte-identical after normalizing timestamps and ids.
- TypeScript notebook: identical except stream chunking (below).
- REST and websocket transcripts match `jupyter server` for kernelspecs,
  contents, sessions, execute, stdin, interrupt, and restart on both
  kernels, after the exclusions below.

nbclient details that decide the bytes:

- `execution_count` counts cells executed in this run, not the kernel's
  counter; blank cells and cells tagged `skip-execution` are untouched.
- `metadata.execution` timestamps are the message header dates
  reformatted to `%Y-%m-%dT%H:%M:%S.%fZ`.
- `display_id` updates reach outputs in earlier cells; `clear_output(wait)`
  defers to the next output.
- `coalesce_streams` defaults to false, so `\r` survives in saved
  output. With it on, adjacent streams merge and `\r` overwrites the line;
  nbclient's backspace handling never runs (its loop condition is false on
  entry), so neither does ours.
- `jupyter execute` without `--output` or `--inplace` writes nothing, and a
  failing run writes nothing. The old `ganymede exec` overwrote its input;
  `execute` now matches Jupyter.
- `language_info` from kernel_info goes into notebook metadata verbatim;
  widget state goes into `metadata.widgets`.

Findings:

- cdp-kernel's stdout and stderr order within one cell is not
  deterministic (nbclient sees the same), and stream chunk boundaries
  depend on timing for any kernel. The parity test compares exactly first,
  then with adjacent same-name streams merged.
- jupyter_client replaces the installed `python3` kernelspec with
  ipykernel's own when ipykernel is importable, so `argv` and
  `kernel_protocol_version` differ from the file on disk. The host reads
  only the file, by design.
- jupyter_server marks notebook cells `metadata.trusted` from its notary.
  Ganymede has no trust store, so JupyterLab may treat outputs as
  untrusted. Open.
- Contents now returns `hash` and `hash_algorithm` (null unless
  `?hash=1`, then SHA-256), and kernelspecs carry
  `kernel_protocol_version`, as jupyter_server 2.11+ does.
- Restarting through the API relaunched the kernel in the server's cwd
  instead of the notebook's directory. Fixed.

Portability:

- Features `zmq` and `subprocess` on kernel-host and engine gate sockets
  and child processes. Without them the protocol, nbformat, kernel host,
  executor, and engine build for `wasm32-unknown-unknown` (with uuid's
  `js` and chrono's `wasmbind`) and `wasm32-wasip1/p2`.
- `e2e/wasm-smoke` runs a notebook on the in-process kernel inside wasm
  under Node's WASI: no sockets, processes, or threads.
- With all features the engine, API, and contents build for
  `aarch64-apple-ios` and `aarch64-linux-android`. The server needs a C
  cross toolchain there (psm, via its dependencies).
- A browser page needs more than a build: tokio's timers and `spawn` need
  a runtime seam (`wasm-bindgen-futures` and browser timers), and a
  kernel reaches the page over a websocket or `postMessage` transport
  behind the existing `Transport` trait. The iPad kernel from the
  ts-on-ipados plan is the same shape: an in-process `Transport`.

## 2026-10-17, release workflow

`.github/workflows/release.yml` packages the `ganymede` binary.

- Trigger: a pushed `v*` tag publishes; pull requests touching the
  workflow and manual runs build and package without publishing, so the
  packaging is exercised before a tag exists.
- The tag must equal `v` + `[workspace.package] version`, and each native
  build checks `ganymede --version` prints that version. Bump the version
  first, then tag. A prerelease tag (`v0.1.0-rc.1`) needs the same string
  in `Cargo.toml`.
- Targets build natively on their own runners, no cross toolchains:
  Linux on `ubuntu-22.04` and `ubuntu-22.04-arm` (older glibc than
  `ubuntu-latest`, so the binary runs on more distributions), macOS
  aarch64 on `macos-latest`, macOS x86_64 cross-compiled on the same
  arm64 runner with Apple's toolchain (not run, Rosetta is not
  guaranteed), Windows x86_64 MSVC. psm's C code needs only the runner's
  compiler.
- Linux binaries are glibc, not musl. Nothing in the dependency tree
  needs OpenSSL (reqwest with rustls is dev-only), so a musl build is
  possible later if a fully static binary is wanted.
- Archives hold `ganymede[.exe]` and README.md in a
  `ganymede-<version>-<target>/` directory. The release job concatenates
  the per-archive `.sha256` files into `SHA256SUMS` and verifies them
  before `gh release create --generate-notes`.
- No code signing or notarization yet: macOS Gatekeeper quarantines a
  downloaded binary (`xattr -d com.apple.quarantine ganymede`).

## 2026-10-17, release tooling, changelog, issue tracking

- `scripts/release.sh <level> [--execute]` wraps cargo-release (version
  bump across the workspace, Cargo.lock, commit, tag, push) and git-cliff
  (CHANGELOG.md). Dry run is the default. It refuses unless HEAD is
  `origin/main`. The release commit goes straight to `main`; it is the one
  commit without a PR.
- cargo-release runs `pre-release-hook` once per crate, so the hook lives
  in `ganymede-cli`'s `[package.metadata.release]`, not `release.toml`.
  Hooks also run in dry runs with `DRY_RUN=true`, so the hook prints the
  changelog then instead of writing it. `CHANGELOG.md` must be tracked
  before the first release: cargo-release commits modified files, not new
  ones.
- The changelog comes from merge commits, offline: GitHub's merge commit
  is `Merge pull request #N from <branch>` with the PR title as its body,
  and a squash merge's subject ends in `(#N)`. Preprocessors turn both
  into `<title> (#N)`; every other commit is dropped. No GitHub token
  needed. A rebase merge would leave no trace and drop the PR.
- release.yml takes the notes from the version's CHANGELOG.md section
  (and fails without one) instead of `--generate-notes`, so the release
  page and the file agree.
- Work is tracked in issues; `pr-issue.yml` fails a PR whose body has no
  `Closes/Fixes/Resolves/Refs #N`. Issue templates: bug, task.
- Install check: `ganymede run -k fake hello` and `ganymede serve` +
  `/api/status` need no kernel, so they are the README's smoke test.

## 2026-10-17, project site and docs

A landing page in `site/` and VitePress docs in `docs/`, both in
cdp-kernel.org's design, built by `.github/workflows/pages.yml` on pull
requests (so a dead link fails the PR) and deployed to GitHub Pages from
`main`. `sketch.md` was split into docs pages and this log moved from
`notes.md` to `docs/notes.md`; the README was rewritten around what exists.

- The site is served at `https://tlockney.github.io/ganymede/` until there
  is a custom domain, so paths are not at the root. `SITE_URL` in
  pages.yml sets it, and `config.mts` derives the docs' base (`/ganymede/docs/`)
  from it. The landing page uses only relative links, so it works under any
  prefix.
- The repository has no root `package.json`; the docs carry their own
  (`docs/package.json`, VitePress only). VitePress skips `node_modules`
  inside its source directory, so it can live in `docs/`.
- VitePress fails the build on links to `.md` files it doesn't publish.
  Links from docs to repository files (`../README.md`, `../validation/`)
  are rewritten to GitHub URLs in `config.mts`, as cdp-kernel does.
- The copied stylesheet's two-column grid used `1fr` columns, whose
  minimum is the content's width, so a long code line widened the page on
  phones. `minmax(0, 1fr)` fixes it; cdp-kernel.org has the same issue.
- The architecture diagrams are hand-written SVG in the same style as
  cdp-kernel's, with a `prefers-color-scheme` block for dark mode. Inside
  an `<img>` they follow the system setting, not the docs' theme switch.
- GitHub Pages must be enabled with "GitHub Actions" as the source before
  the first deploy.

## 2026-10-17, survey of other Jupyter clients

[Other Jupyter clients](clients.md) lists frontends beyond JupyterLab and
VS Code and whether each could run against Ganymede.

- Most non-browser clients only need token auth, `/api/kernelspecs`,
  `/api/kernels` and the channels websocket, which Ganymede already serves.
  Zed's REPL is one: it reads `JUPYTER_SERVER` and `JUPYTER_TOKEN` and sends
  the token in the websocket query.
- Clients that load server HTML pages (Juno Connect wants the classic
  `/tree` page, jupynium drives a browser UI) are blocked on pages Ganymede
  doesn't serve, not on the API.
- nteract is now a Tauri app on a Rust daemon that talks ZMQ to local
  kernels; it has no Jupyter server client. Its design records are useful
  for our own UI.
- papermill launches kernels itself; to drive Ganymede it needs an engine
  using `jupyter_server`'s `GatewayKernelManager`, which also exercises
  Ganymede as a kernel gateway.

## 2026-10-17, install script

`site/install.sh` wraps the [Install](install.md) steps:
`curl -fsSL https://tlockney.github.io/ganymede/install.sh | sh`.

- It lives in `site/` so Pages serves it at the site root; the raw GitHub
  URL works too. Everything runs inside `main()`, so a truncated download
  executes nothing.
- The latest version comes from the `/releases/latest` redirect, as in the
  manual steps. With no release published the redirect goes to
  `/releases`, and the script says so and points to `cargo install`.
- The cdp-kernel step is on by default and never fatal: without Node 24+
  or with an `npm install -g` that needs root, it prints the commands
  instead.
- `GANYMEDE_DOWNLOAD_BASE` swaps the download base, so
  `scripts/test-install.sh` tests the script against a stub release over
  `file://` on Linux and macOS, under sh, bash, and dash, without a
  published release.

## 2026-10-17, `ganymede lab install`

JupyterLab's frontend without Python (#15): `ganymede lab install
[--version X]` downloads the `jupyterlab` wheel from PyPI, checks its
SHA-256, and unpacks `<name>.data/data/share/jupyter/lab/{static,schemas,themes}`
plus the dist-info `LICENSE` into `ganymede/lab/<version>/` under the local
data dir. `ganymede serve` without `--static-dir` serves the newest one, or
`--lab-version` / `lab_version`, and only the API when none is installed.

- The default is 4.6.4 (latest 4.x on PyPI at the time). Its wheel's
  SHA-256 is also pinned in the binary, so a changed PyPI listing for the
  default fails the install; other versions trust PyPI's JSON
  (`/pypi/jupyterlab/<v>/json`, the `bdist_wheel` entry of `urls`).
- The wheel is about 17 MB and unpacks to 18 MB. Fetching on demand keeps
  it out of the release archives; bundling would make offline installs work.
- HTTP is `ureq` 3 (blocking, run under `spawn_blocking`) with rustls on
  `ring` and `rustls-platform-verifier`, so there is no OpenSSL, no
  aws-lc/cmake/nasm in the release build, and the OS trust store is used:
  a corporate or sandbox proxy's CA installed there just works (it did
  here, through the agent proxy, with no extra flags). ureq reads
  `HTTPS_PROXY`/`ALL_PROXY` by default. The webpki roots compiled in by
  ureq's `rustls` feature are unused.
- `zip` 9 with only `deflate-flate2-zlib-rs`: wheels use stored and
  deflate entries. `enclosed_name()` drops absolute and `..` paths.
- Installs unpack into `.<version>-*` beside the target and are renamed
  into place, so a bundle counts as installed once `static/index.html`
  exists and dot-directories are ignored. `lab.rs` already derives
  `schemas/` and `themes/` from `static/`'s parent, so the installed
  layout needed no server changes beyond picking the directory.
- The lookup (`lab::installs_dir`, `lab::installed`, version ordering)
  lives in `ganymede-server`, which both binaries share; the network and
  zip code stay in the CLI, away from the wasm-built crates.
- `dirs::data_local_dir()` rather than `data_dir()`: the same on Linux and
  macOS, but `%LOCALAPPDATA%` on Windows, so 18 MB doesn't roam.
  `GANYMEDE_DATA_DIR` overrides it; the tests use that.
- The network test (`tests/lab.rs`) runs only with `GANYMEDE_E2E_LAB=1`,
  which CI's e2e job sets on Linux and macOS; `cargo test` stays offline.

## 2026-10-17, cookie login

JupyterLab in a browser now runs notebooks against `ganymede serve`
without the `WS_TOKEN=1` workaround: its websockets authenticate with the
login cookie. Checked against jupyter_server 2.21.1 (tornado 6.5) and
JupyterLab 4.6 in headless Chromium.

What jupyter_server does, and Ganymede now matches (the server e2e
transcript records each of these, cookie attributes included):

- Any token-authenticated request that arrives without a valid login
  cookie gets one, API calls included. The cookie is
  `username-<host with non-alphanumerics as dashes>`, a tornado version 2
  signed value holding a JSON user, HttpOnly, `Path=<base_url>`, expiring
  in 30 days; signatures are accepted for 31.
- A correct token wins over the cookie. An invalid cookie is answered 403
  and cleared (`name=""` with a past expiry).
- Pages redirect a browser with no login to `login?next=<url-encoded
  path and query>`; the API answers `403 {"message": "Forbidden"}`.
- `/login` renders a form with a hidden `_xsrf` field and sets the `_xsrf`
  cookie if missing. The POST needs that field (403 without it), answers
  401 for a wrong token, and 302s to `next` with the cookie. `next` must
  be a path under `base_url`, else `base_url`. `/logout` clears the cookie
  and, being a rendered page, also sets `_xsrf`.
- XSRF applies to methods other than GET, HEAD, and OPTIONS that did not
  send the token, and runs before the auth check, so an unauthenticated
  POST gets the XSRF message, not "Forbidden". The token comes from
  `_xsrf` (query or form), `X-XSRFToken`, or `X-CSRFToken`. Messages:
  `'_xsrf' argument missing from POST`, `'_xsrf' argument has invalid
  format`, `XSRF cookie does not match POST argument`. A cross-origin
  request fails first with 404. `_xsrf` values are `2|<4-byte
  mask>|<masked token>|<time>` and compare by the unmasked token; any
  other numbered version is invalid, and an unnumbered value is a
  version 1 token, hex or verbatim. `_xsrf` is not HttpOnly, because
  JupyterLab reads it to set the header.
- Cookie-authenticated API requests from another origin get 404, as
  jupyter_server's `check_origin` does when the token was not sent.
- The page config still carries the token, so JupyterLab's REST calls
  are token-authenticated and need no XSRF header; only websockets and
  `<img>` loads (kernelspec logos, which were 403 before) rely on the
  cookie.

Choices where Ganymede differs:

- The cookie secret is persisted, as jupyter_server does, in
  `jupyter_cookie_secret` in the runtime dir (owner-only), with the same
  derivation (HMAC-SHA256 of the file with an empty message, the no-password
  case). `cookie_secret_file` in the config moves it. Logins survive a
  restart; changing the token does not log browsers out, also as in
  jupyter_server. If the file can't be written, the key is per process.
- The login cookie adds `SameSite=Lax`, which jupyter_server does not
  send. The e2e transcript ignores that attribute.
- A page URL with a correct `?token=` sets the cookie and redirects to the
  same URL without the token. jupyter_server renders the page with the
  token still in the address bar. API paths and websocket upgrades keep
  the parameter.
- `next` is never followed to another origin, even one in
  `allow_origins`.
- With auth disabled there is no XSRF check. jupyter_server still checks
  unless `disable_check_xsrf` is set.
- `/api/me` still reports one anonymous identity; the cookie's user is
  not read back.

Findings:

- Python's cookie formatting quotes values containing `=` (base64
  padding), so jupyter_server's `Set-Cookie` sometimes has a quoted value
  and sometimes not. Browsers send the quotes back; the parser strips
  them.
- tornado's `_websocket_mask` needs a 4-byte mask, so a shorter mask in a
  version 2 `_xsrf` is "invalid format", not a mismatch. The transcript
  caught this.
- `form_urlencoded::Serializer::new` with a target that already ends in
  `?` adds a `&`; build the query separately.

## 2026-10-17, notebook trust

Notebooks are signed and marked `trusted` as `jupyter_server` does, so
JupyterLab runs the HTML and JavaScript outputs of a notebook it reopens
(issue #18). `ganymede-contents` has `trust::Notary`, `FsContents` signs on
save and marks cells on read, the server serves `POST
/api/contents/{path}/trust`, and `ganymede trust` replaces `jupyter trust`.

- Ganymede shares Jupyter's store instead of keeping its own:
  `notebook_secret` and `nbsignatures.db` in the Jupyter data dir
  (`--notary-dir` / `notary_dir` moves them). A user moving from Jupyter
  keeps their trusted notebooks, and `jupyter trust` and `ganymede trust`
  agree; `parity.rs` checks both directions.
- There is no mature pure-Rust SQLite (`turso_core` is a pre-release), so
  the database goes through `rusqlite` with SQLite compiled from C. It sits
  behind the contents crate's `sqlite` feature, which only the server
  enables: `ganymede-contents` alone still checks for iOS and Android with
  no C cross compiler, and the engine, which does not depend on it, still
  builds for wasm. Without the feature signatures are kept in memory, as
  nbformat does when `sqlite3` is missing.
- The signature is an HMAC-SHA256 over nbformat's `yield_everything`: for
  each object, keys in sorted order each followed by its value; strings as
  UTF-8; everything else as Python's `str()` (`True`, `None`, `1e-05`, so
  the writer's float repr is reused); `metadata.signature` skipped. List
  items are concatenated with no separator, so a source split into lines
  hashes the same as the joined string. The key is the secret file's
  bytes, trailing newlines included. Fixtures from
  `tests/fixtures/generate_trust.py` pin it.
- What is hashed is the notebook as served, without `trusted`. On save
  Ganymede hashes the notebook as it will be read back rather than the
  client's JSON, which is what makes the save-then-reopen round trip
  verify. jupyter_server hashes the client's dict, so a `trusted` flag on a
  markdown cell, which nbformat drops on read, would make its own
  signature miss.
- The save rule is `check_cells`: a code cell is trusted when its
  `metadata.trusted` is truthy (popped either way) or no `execute_result`
  or `display_data` output has a key besides `output_type`,
  `execution_count` and `metadata`. An empty `data: {}` counts as unsafe.
  JupyterLab sets `trusted` on cells it ran, which is how "every output was
  produced in this session" reaches the server.
- Signatures are content-addressed, so a byte-identical copy of a trusted
  notebook is trusted too. The JupyterLab check in
  `validation/trust.mjs` changes the copy's metadata before saving it
  untrusted, or the control would pass for the wrong reason.
- nbformat does not create the data dir before writing the key, so
  `jupyter server --NotebookNotary.data_dir=<missing dir>` answers 500 on
  every notebook read. Ganymede creates it; the parity test creates it for
  the reference.
- `jupyter trust --reset` writes 1024 raw random bytes as the new key; the
  key Ganymede generates (then and on first use) is the base64 text nbformat
  writes on first use. Any bytes work as an HMAC key.
- Checked with JupyterLab 4 in headless Chromium against `ganymede serve`:
  after save and reopen in a fresh page, the HTML output's `<script>` and
  the `application/javascript` output both run; in an edited copy saved
  without `trusted` flags neither does.

## 2026-10-17, releases through a PR

- The first `release.sh --execute` could not push the release commit and
  tag to `main`: Claude sessions may push only their own branch (HTTP 403
  from the proxy), and direct pushes also skip the PR-links-issue rule.
- Now cargo-release only bumps and commits (`tag = false`, `push = false`
  in release.toml). The script moves that commit to `release/vX.Y.Z`,
  resets local `main`, pushes, and with `gh` opens an issue and a PR that
  closes it. A release can be cut by merging a PR from the GitHub app.
- `tag-release.yml` runs on pushes to `main` that touch `Cargo.toml`. It
  tags only when the workspace version has a CHANGELOG.md section and no
  tag, so dependency changes and the untagged 0.0.1 are ignored.
- A tag pushed with `GITHUB_TOKEN` triggers no workflows, but
  `workflow_dispatch` is exempt, so tag-release runs
  `gh workflow run release.yml --ref vX.Y.Z` (needs `actions: write`).
  Dispatched on a tag ref, `github.ref_type` is `tag`, so release.yml's
  version check and publish job behave as on a pushed tag.

## 2026-10-17, kernels that never answer under parallel starts (#25)

`e2e (ubuntu-latest)` failed with "timed out waiting for
kernel_info_reply" on a PR that changed no Rust. Locally, 40 parallel
`ganymede run -k cdp-node` failed about 1 in 10, as "timed out", "kernel
died", or "zmq: Provided sockets combination is not compatible"; the
kernels e2e binary failed 2 of 12 runs four at a time.

- Ports are free when picked and taken by whoever binds first. Until the
  kernel binds, a host's connection can land on another socket on that
  port: another host's free-port probe, or another kernel's PUB.
- The `zeromq` crate reconnects SUB sockets but not DEALERs. A DEALER
  whose first peer went away stayed unconnected, and every send failed
  with "Not connected to peers" until the timeout. DEALERs now reconnect
  on a failed send, and a failed handshake is retried, not fatal.
- `zeromq`'s connect retries a refused port with backoff up to 5 s, and
  cutting a handshake short with its `connect_timeout` can leave the
  kernel's ROUTER holding our routing id, so later connections with it are
  ignored (attach tests timed out that way). A plain TCP probe, retried
  every 20-250 ms, now waits for the port before zeromq connects.
- `ZmqTransport::connect` returns at once; each socket connects in its
  task, so the startup timeout bounds the whole start. Shell sends nothing
  until stdin and iopub are connected (an `input_request` needs the stdin
  socket, a first request's outputs need iopub). The nudge keeps sending
  `kernel_info_request` on shell until iopub has delivered something,
  since the SUB socket can connect after shell and control answered.
- Two kernels can still be given the same port; the loser exits or never
  answers. `Relaunch::start` (engine start and restart) launches again on
  fresh ports, up to 3 times, after `KernelDied` or a startup timeout.
- Ports are picked at random from 20000-32767, below the ephemeral
  ranges, so a reconnecting socket cannot be given the kernel's port as
  its own source port.
- Connecting lazily exposed a bug in the API's restart: the new kernel
  reuses the id and so the connection file path, and dropping the old
  launch afterwards deleted the new kernel's file. ipykernel that had not
  read it yet wrote its own with other ports. The old launch is now
  dropped before the new kernel starts.
- Result: 0 failures and 0 relaunches in 320 parallel CLI starts, and the
  whole `ganymede-cli` e2e suite passes 12 of 12 runs four at a time. The
  API's own launch path gets the reconnects but not the relaunch yet.

The server parity test later failed once on CI with jupyter_server
answering a DELETE with 400. Its contents manager sends files to the trash
by default, and send2trash creates `~/.local/share/Trash` by checking that it
exists and then calling `makedirs`. On a fresh runner the cdp-kernel and
ipykernel parity tests delete at the same moment, both see no trash
directory, and one `makedirs` fails. Paired deletes into a fresh home failed
1 time in 30 locally. The reference server now runs with
`--FileContentsManager.delete_to_trash=False`, which also matches ganymede,
which deletes for good.

## 2026-10-17, prebuilt JupyterLab extensions from wheels

`ganymede lab extension install|list|remove` (#16) unpacks
`*.data/data/share/jupyter/labextensions/**` from a package's PyPI wheel
into `ganymede/labextensions/` under the data dir, which `serve` searches
after `--labextensions-path` and before the Jupyter data dirs. `ganymede
lab install` installs `jupyterlab_widgets` and `anywidget` too, unless
`--no-default-extensions`. cdp-kernel's counter widget from its
`docs/widgets.md` renders in JupyterLab served this way, counts clicks,
and the kernel reads the count back (`validation/widgets.mjs`).

- anywidget does not require `jupyterlab_widgets` directly: it requires
  `ipywidgets`, which has no extension and requires `jupyterlab_widgets`.
  So "follow only dependencies whose wheel carries an extension" stops too
  early. The rule used: walk `requires_dist` from PyPI's JSON (no
  downloads), without `extra` markers, descending only through packages
  classified `Framework :: Jupyter`; download and unpack those classified
  `Framework :: Jupyter :: JupyterLab :: Extensions`. For anywidget that
  is about 20 JSON requests (ipython's dependencies are checked and
  dropped) and two wheels, under a second here. A named package is always
  downloaded and must carry an extension.
- Dependencies get their latest release; specifiers like `~=3.0.17` are
  not checked. Other markers (`sys_platform`, ...) are ignored, which can
  only add a frontend package.
- Only pure (`py3-none-any`, `py2.py3-none-any`) wheels are picked, which
  also applies to `jupyterlab` now.
- Each package's record is `labextensions/.packages/<name>.json` (name,
  version, extension directories, and the package that pulled it in).
  `find_extensions` skips it because it has no `package.json`, as it skips
  the `.<name>-*` staging directories. Remove deletes only the named
  package's extensions; dependencies stay.
- An extension is a top-level directory, or two levels for `@scope/name`.
  Reinstalling drops extensions the new version no longer ships; one that
  another package had installed is taken over and dropped from that
  package's record.
- `jupyterlab_widgets` ships a settings schema under its extension's
  `schemas/`; `/lab/api/settings` already lists it
  (`@jupyter-widgets/jupyterlab-manager:plugin`).
- `lab::data_dir()` is now the shared root for `lab/` and
  `labextensions/`.

## 2026-10-17, `ganymede lab`

One command to start JupyterLab (#17): `ganymede lab [--no-browser]
[serve flags]` in the notebook directory.

- Command shape: `lab` already had `install/list/remove`, so the start
  flags are flattened onto `lab` itself with an optional subcommand and
  clap's `args_conflicts_with_subcommands`. Bare `ganymede lab` matches
  `jupyter lab`; `ganymede lab list --port 1` is a usage error, not a
  silently ignored flag. A `lab start` subcommand would have been the
  same code with worse parity. The start code is in its own module
  (`lab_start.rs`) so `lab.rs` keeps only the bundle management.
- The server side is `serve::run_with(cfg, Startup)`; `run` is
  `run_with` with the default. `Startup` carries `port_retries`,
  `require_lab`, and an `on_listening(url)` callback, so the browser is
  opened by the CLI after the port is bound and nothing in the server
  knows about browsers.
- Port fallback follows jupyter_server's `port_retries` (default 50) but
  sequentially, without its random ports after the first few, and also
  when `--port` was given. Only
  `AddrInUse` moves on; any other bind error fails at once. `--port 0`
  never retries. `serve` keeps failing on a taken port.
- The printed URL is `/lab?token=…`, as `jupyter lab` prints. The page's
  `strip_token` middleware turns that into the login cookie and a
  redirect to `/lab`, so the browser opened on it is logged in.
- The browser is opened with `open` (macOS), `xdg-open` (elsewhere on
  Unix), or `explorer` (Windows), spawned with null stdio and reaped on a
  thread. A missing opener or a failing exit is a warning; the URL is on
  stdout. `$BROWSER` is not consulted, unlike Python's `webbrowser`.
- No JupyterLab installed (or a `--static-dir` that isn't a Lab bundle)
  fails before binding, naming `ganymede lab install`. The "serving the
  API only" log moved from `installed_lab` to the caller so `lab` doesn't
  print it right before failing.
- The test (`tests/lab_start.rs`) needs no network: `Lab::detect` only
  needs `static/index.html` containing `jupyter-config-data` and a
  `package.json` with a `jupyterlab` key, under
  `$GANYMEDE_DATA_DIR/lab/<version>/`. It holds a port open to check the
  fallback.

## 2026-10-17, workstation install with no Python

CI's `workstation` job (#20) follows [Install](install.md) on
`ubuntu-latest` and `macos-latest` with Python hidden, through
`scripts/test-workstation.sh <binary>`: the PR's release build, packaged
as `release.yml` packages it and served over `file://` with
`GANYMEDE_DOWNLOAD_BASE`, installed with `curl ... | sh -s -- --version`;
then `npm install -g @tlockney/cdp-kernel`, `cdp-kernel install --runtime
node`, `ganymede kernelspec list` (must show `cdp-node`), `ganymede run -k
cdp-node`, `ganymede lab install`, and `ganymede lab --no-browser` in a
notebook directory. Playwright then runs `validation/login.mjs` (the
printed URL lands on `/lab` with the login cookie, and the cookie alone
lists `cdp-node`), `trust.mjs` with a TypeScript cell (`jupyter.html`
with a `<script>`, plus a raw `application/javascript` bundle), and
`widgets.mjs` with `RELOAD=1`.

- Hiding Python: `PATH` is a shim dir, `~/.local/bin`, npm's global bin,
  and a clean dir of symlinks to node, npm, npx, curl, tar, gzip,
  sha256sum/shasum, and coreutils. Shims named `python`, `python3`,
  `pip`, `pip3`, and `jupyter` log the call and exit 1. They miss
  absolute paths, so a sampler also logs, every 0.5 s, any process named
  `python*` that descends from the script. Sampling all processes would
  fail on runners, which run Python daemons of their own. Both were
  checked to fire by calling `python3` and `/usr/bin/python3` on purpose.
- The script sets `HOME` to a new directory, as on a fresh machine, and
  unsets the `XDG_*`, `JUPYTER_*`, and `GANYMEDE_DATA_DIR` variables, so
  the kernelspec, JupyterLab, and extensions land under it and a local run
  touches nothing else. Playwright's browsers are found through
  `PLAYWRIGHT_BROWSERS_PATH`, defaulted from the real `HOME`.
  `NPM_CONFIG_PREFIX` points npm's global installs into that `HOME`;
  without it they go next to setup-node's `node`, which is not on the
  clean `PATH`.
- `install.sh` installs cdp-kernel itself when Node 24 is present; the
  explicit npm and `cdp-kernel install` steps from "A kernel" then run
  again, which also checks that both are idempotent.
- npm 11 (Node 24.22) skips install scripts not listed in `allowScripts`
  and warns about esbuild's postinstall. cdp-kernel runs without it.
- Widgets after a reload need no saved widget state: JupyterLab's widget
  manager asks the running kernel (`comm_info_request` for
  `jupyter.widget`, then each model's state), and cdp-kernel answers, so
  the reopened page shows `count is 3`. The saved notebook holds only the
  `application/vnd.jupyter.widget-view+json` output.
- The Playwright tooling (`npm ci`, `playwright install --with-deps`) is
  set up before Python is hidden; it is the test harness, not the
  workstation.
- On macOS the script relies on `/usr/bin/shasum` (a Perl script with
  an absolute shebang) and BSD `ps`.
- JupyterLab saves on Cmd+S on macOS, where Playwright's Chromium reports a
  Mac platform, so `Control+s` silently does nothing there. The scripts press
  `ControlOrMeta+s`.

## 2026-10-17, the workstation install page

[Install on a workstation](workstation.md) (#21) is the numbered path from
a clean Linux or macOS machine to JupyterLab running TypeScript, for a
reader who has never installed Jupyter. Its commands are the ones
`scripts/test-workstation.sh` runs, and the page's source comment lists
where the two differ and why.

- To cover the page, the script gained three steps: `ganymede execute
  first.ipynb --inplace` on the page's first-notebook cell (an `npm:`
  import of date-fns and `jupyter.display` of `jupyter.md`; the output is
  checked in the saved file), `ganymede serve` with `validation/vscode.cjs`
  replaying the VS Code Jupyter extension's requests, and the uninstall
  (`ganymede lab remove`, `lab extension remove`, `kernelspec remove`,
  `npm uninstall -g`, `rm ~/.local/bin/ganymede`). It also runs
  `ganymede kernelspec paths` and `ganymede info`, which the
  troubleshooting section uses. The full script passed locally on Linux
  x86_64 with Node 24.22 and a debug build.
- `cdp-kernel list` and `cdp-kernel uninstall` (cdp-kernel #251) are on
  cdp-kernel's main but not in 0.5.0 on npm, so the page gives
  `ganymede kernelspec remove cdp-node` and mentions them as the
  alternative for later releases. Add them to the script once released.
- The VS Code Jupyter extension doesn't need the Python extension for an
  existing server: its `package.json` (2026.6.1) has no
  `extensionDependencies`, and its `extensionPack` is Keymap, Renderers,
  Cell Tags, and Slide Show. Not checked by running VS Code.
- `ganymede serve` doesn't fall back to another port as `lab` does; it
  exits with `binding 127.0.0.1:8888: Address already in use`.
- `ganymede lab install` honours `HTTPS_PROXY` through ureq; cdp-kernel's
  `npm:` imports run `npm install`, so npm's proxy settings apply.

## 2026-10-17, docs unstyled on the custom domain

Pages got the custom domain `ganymede-server.com`, and
`tlockney.github.io/ganymede/` now redirects there. The docs were still
built with `SITE_URL=https://tlockney.github.io/ganymede`, so VitePress
wrote every asset and link under `/ganymede/docs/`, which 404s on the
custom domain: the docs rendered without styles. The landing page uses
relative paths and kept working. `SITE_URL` (pages.yml, and the default
in `config.mts`) is now the custom domain, and the install-script URLs
point at it directly instead of through the redirect.

- A project page (`<user>.github.io/<repo>/`) and a custom domain serve
  the same files at different path prefixes. Anything with an absolute
  base path has to be rebuilt when the domain changes; relative paths
  survive it.
- With an Actions deployment the custom domain lives only in the Pages
  settings, not in a `CNAME` file in the repository, so nothing in the
  build sees a change there.
- The deploy job now fetches the landing page, `install.sh`, a docs page,
  and the stylesheet that page links from `SITE_URL` after each deploy,
  so a base path that doesn't match the domain fails the deploy instead
  of shipping unstyled docs. Run against the old deploy, the stylesheet
  check 404s, as it should.
- No `site/CNAME`: GitHub ignores a `CNAME` file in an Actions-deployed
  artifact, so it would not keep the domain set; the Pages settings are
  the only place it lives.

## 2026-10-17, auth off no longer means open to every web page (#41)

With `--token ""` the API middleware skipped every check, so any page in
the user's browser could create files, trust notebooks, and start kernels
with a cross-site `text/plain` POST (no preflight), and a DNS-rebinding
page could read files. jupyter_server keeps two protections with auth off,
and Ganymede now matches them:

- XSRF applies to every state-changing request that did not send the
  token, whether or not auth is on (`check_xsrf_cookie` only skips
  token-authenticated requests). With auth off, a client that is not a
  browser must send `_xsrf` as a cookie and an `X-XSRFToken` header, or
  run with a token. Reads and websocket upgrades are unaffected; the
  websocket handlers already check `Origin`.
- `Host` must be a loopback address or a name in `local_hostnames`
  (default `localhost`) unless `allow_remote_access` is on. It defaults to
  on only when `ip` is not loopback, as `_default_allow_remote` does; a
  host name for `ip` counts as loopback when every address it resolves to
  is. The check runs on every route, pages included, and a request with no
  `Host` passes, as tornado treats it as local.
- A rebinding page is same-origin with itself (its `Origin` and `Host`
  agree), so the `Origin` check cannot catch it; only `Host` shows the
  request was meant for another name.
- A reverse proxy in front of a loopback-bound server needs
  `--allow-remote-access` or its public name in `local_hostnames`, unless
  it rewrites `Host`.

## 2026-10-18, numbers round-trip on save (#72)

The workspace `serde_json` had only `preserve_order`. Its default float
parser is not correctly rounded, so reading a notebook and writing it back
changed some doubles (5,389 of 20,000 random finite doubles in the new
test), saves drifted further each pass, and integers beyond u64 became
floats. Trust signatures hash the re-serialized numbers, so a notebook
Jupyter signed could read back as untrusted. Both `float_roundtrip` and
`arbitrary_precision` are on now.

- `arbitrary_precision` keeps each number's source text in `Value`, so the
  writer's `write_f64` hook no longer sees numbers from a parsed file;
  `PythonFormatter::write_number_str` does the work instead: floats go
  through `python_float_repr`, integers are written as read (`-0` as `0`,
  as Python's `int` does).
- `Number::is_f64` is true only for text with `.`, `e` or `E`, so the
  signing code's int/float split matches Python's for big integers.
- `float_roundtrip` still matters with `arbitrary_precision`: it governs
  deserializing straight into typed `f64` fields.
- `#[serde(flatten)]` structs with typed integer fields still deserialize
  from text with `arbitrary_precision` on (checked on `ExecuteResult`);
  older serde releases failed there.
- A JSON number Python reads as `inf` (`1e400`) keeps its text; Python
  would write the non-JSON `Infinity`.
- The Python e2e notebook's `JSON` display now carries 17-digit floats,
  which fails the nbclient parity test without the fix.
