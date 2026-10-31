# Other Jupyter clients

Jupyter frontends and clients other than JupyterLab and VS Code (see
[JupyterLab and VS Code](frontends.md)), and whether each could run
against Ganymede. Surveyed in October 2026; none of these are in the
automated tests.

## What Ganymede offers a client today

As of this survey:

- REST: `/api/kernels`, `/api/kernelspecs`, `/api/sessions`, `/api/contents`,
  `/api/status`, `/api/me`, `/api/config`, `/api/events`, and JupyterLab's
  `/lab/api/*`. Websocket at `/api/kernels/{id}/channels`.
- Terminals (**Unreleased.** A build of `main` only): `/api/terminals` and the
  websocket at `/terminals/websocket/{name}`.
- Auth: token in header or `?token=`, or the login cookie a token or the
  `/login` form sets, with `jupyter_server`'s `_xsrf` check.
- CORS via `allow_origins`. Cross-origin clients send the token; the login
  cookie works cross-origin only with `allow_credentials`.
- Kernel connection files are written to `JUPYTER_RUNTIME_DIR`, so a ZMQ
  client can attach to a kernel Ganymede launched.
- Not served: classic Notebook / Notebook 7 pages (`/tree`, `/notebooks`),
  `/lsp/*`, collaboration rooms (`/api/collaboration`),
  nbconvert exporters, server extensions.

So a client works today if it speaks REST/websocket with a token and needs no
HTML pages from the server.

## Survey

"Last commit" is the default branch's latest commit date, read from a shallow
clone on 2026-10-17. App Store apps show the latest release instead.

### Mobile

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **Juno Connect** (iPad, $4.99) | Stale. Last release 1.6.13, 2022-09-18 | REST/websocket to a remote server, over HTTP(S) or SSH tunnel | **Unknown, likely blocked.** Its JupyterLab 4 workaround is pointing it at `nbclassic/tree`, so it seems to need the classic Notebook pages and maybe a login form. Needs a device test. |
| Juno (iPad) | Same developer | Runs Python on the device | No server involved. |
| Carnets (iOS) | Last commit 2024-02 | Bundled Python and Jupyter on the device | No server involved. |

There is no maintained native Android Jupyter client; people use a browser.

### Desktop apps

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **nteract** (originally backed by Netflix) | Yes, rewritten. Last commit 2026-10-16. Now a Tauri + React app with a Rust daemon (`runtimed`), Automerge sync and hosted "rooms"; Electron version is gone | Direct ZMQ to local kernels via `jupyter-zmq-client` / `jupyter-protocol`. No Jupyter server client | No. It replaces the server rather than using one. Relevant as prior art (same Rust crates Ganymede's notes list) and as a place to run cdp-kernel. |
| JupyterLab Desktop | Yes, last commit 2026-10-14 | Bundles a Python env; can also open a remote server URL, which loads that server's JupyterLab page | Same as JupyterLab in a browser, which works with cookie login. No separate test needed. |
| PyCharm / DataSpell (JetBrains) | Yes, commercial | REST/websocket to a server by URL + token | Likely works; it is a token client like VS Code. Manual test only. |
| Positron (Posit) | Yes, last commit 2026-10-17 | Its own kernel supervisor (Kallichore, Rust) over ZMQ; no Jupyter server client | No. Prior art for a Rust kernel supervisor. |
| Google Colab "local runtime" | Extension last commit 2021-08 | Needs the `jupyter_http_over_ws` server extension (an HTTP-over-websocket proxy) | No, and not worth adding. |

### Editors and terminal

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **Zed REPL** | Yes, last commit 2026-10-17 | Rust; `jupyter-websocket-client` from runtimed. Set `JUPYTER_SERVER` and `JUPYTER_TOKEN`; it calls `/api/kernelspecs`, `/api/kernels`, then `/api/kernels/{id}/channels?token=` | **Should work today.** Also has an early `.ipynb` notebook UI. |
| **jpterm** (successor to nbterm) | Yes, last commit 2026-10-15 | `--server URL`: REST/websocket. Collaborative mode needs Y rooms (Jupyverse) | Should work with `--no-collaborative`. Scriptable, so CI-friendly. |
| **emacs-jupyter** | Yes, last commit 2026-08-13 | ZMQ locally, or REST/websocket via `jupyter-server-*` commands | Should work over REST. |
| Emacs IPython Notebook (EIN) | Slow, last commit 2025-12 | REST/websocket, `/api/contents` | Probably works; lower priority than emacs-jupyter. |
| molten-nvim | Yes, last commit 2026-07-02 | ZMQ locally, or `:MoltenInit <server URL>` over REST/websocket | Should work over REST. |
| jupynium.nvim | Yes, last commit 2026-09-20 | Drives a browser running Notebook/JupyterLab via Selenium | Needs the classic or Lab UI in a browser; Lab works, the classic UI isn't served. |
| euporie | Yes, last commit 2026-10-07 | ZMQ only: local kernelspecs or `--connection-file` | Can attach to a Ganymede-launched kernel's connection file. |
| jupyter console / qtconsole | Yes (2026-09 / 2026-03) | ZMQ, `--existing` connection file | Same as euporie. |

### Agent CLIs

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **nb-cli** (`nb`, jupyter-ai-contrib) | Yes, 0.0.10, last commit 2026-09-09, BSD-3 | Remote mode: jupyter-server-documents or jupyter-collaboration if served, else the contents API and kernel websocket. Gateway mode: kernel-gateway subset, notebook on local disk | **Works**, tested 2026-10-31 on cdp-kernel and ipykernel with [`validation/nb-cli.sh`](https://github.com/tlockney/ganymede/blob/main/validation/nb-cli.sh): `create`, `read`, `cell add/update/delete`, `execute` (kernel state kept across calls), `search --with-errors`, `output clear`, `--restart-kernel`, and `execute --gateway`. Connect with `nb connect --server URL --token T`: auto-detect runs `jupyter server list`. `nb create` looks the kernel up on local disk, so the kernelspec must be installed locally too (or under `VIRTUAL_ENV`). In gateway mode pass `--kernel-id`; without it nb takes the first kernel listed, whatever its language. |

### Embeddable / libraries

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **Datalayer jupyter-ui** (`@datalayer/jupyter-react`) | Yes, last commit 2026-10-06, MIT | React components over `@jupyterlab/services`: REST/websocket with token; optional Yjs collaboration | **Should work today** in non-collaborative mode, cross-origin with `allow_origins`. Its README notes comms "need to be fixed", so widgets may not. ganymede-ui began on it and dropped it. |
| Thebe | Yes, last commit 2026-06-22 | `@jupyterlab/services` against a server or Binder | Should work with token + CORS. Live code cells in static pages. |
| JupyterLite | Yes | No server; in-browser kernels | Not a client; precedent for the iPad profile. |
| Voilà | Yes, last commit 2026-09-04 | A `jupyter_server` extension | No (Python server extension). |

### Headless

| Client | Maintained | Talks to | Against Ganymede |
|---|---|---|---|
| **papermill** | Yes, last commit 2026-03-08 | `nbclient` → `jupyter_client`: launches kernels locally and talks ZMQ. No server needed | Two ways to test: (1) run papermill on cdp-kernel's kernelspec and compare with Ganymede's own executor (a kernel and output-parity test, no server); (2) a custom engine using `jupyter_server`'s `GatewayKernelManager` pointed at Ganymede, which drives Ganymede's REST/websocket API (Elyra ships such an engine). |
| nbclient / `jupyter execute` | Yes, last commit 2026-06-05 | Same as papermill | Already the e2e oracle. |
| `jupyter_server --gateway-url` / Enterprise Gateway clients | Gateway client yes; Kernel Gateway last commit 2024-03 | Remote kernels over `/api/kernels`, `/api/kernelspecs` and channels | Should work: Ganymede already serves the gateway subset. One test covers papermill-via-gateway, Elyra and JupyterLab-on-Python-with-Ganymede-kernels. |
| Datalayer `jupyter-kernel-client`, `jupyter-mcp-server` | Yes | REST/websocket; MCP server also uses collaboration rooms for notebook edits | Kernel client should work; MCP notebook editing needs Y rooms. |

## Worth trying, in order

1. **Zed REPL.** A native, maintained, Rust REST/websocket client. Two env vars,
   no Ganymede changes expected. Quick confidence that a non-browser client works.
2. **papermill**, both ways: kernel-level parity on cdp-kernel, and the
   `GatewayKernelManager` engine against Ganymede. The second also proves
   Ganymede works as a Jupyter gateway, which opens the door to every Python
   Jupyter server using it for kernels.
3. **Datalayer jupyter-ui.** The cheapest way to see a React notebook on
   Ganymede, cross-origin with a token. ganymede-ui started on it and moved
   to JupyterLab's packages directly. Can be automated with the existing
   Playwright harness.
4. **Juno Connect.** The only iPad client. Expect gaps (classic `/tree` page,
   login form). Needs an iPad; cookie login has landed.
5. **jpterm** (`--no-collaborative`) as a cheap scripted CI client, and
   **`jupyter console --existing`** on a Ganymede-launched kernel to check that
   two clients can share one kernel.

Skip: nteract, Positron, euporie, Colab local runtime, JupyterLab Desktop
(covered by JupyterLab), Voilà.

## Sources

- datalayer/jupyter-ui: https://github.com/datalayer/jupyter-ui
- nteract: https://github.com/nteract/nteract (README and `docs/runbooks/remote-workstation.md`)
- runtimed crates: https://github.com/runtimed/runtimed, https://docs.rs/runtimelib
- Juno Connect: https://apps.apple.com/app/id1315744137, https://juno.sh/troubleshooting-juno-connect/
- Zed REPL: https://zed.dev/docs/repl.html, `crates/repl/src/repl_store.rs` and `kernels/remote_kernels.rs`
- jpterm: https://github.com/davidbrochart/jpterm (`docs/usage/TUI.md`)
- euporie: https://euporie.readthedocs.io (`pages/kernels.rst`, related projects page)
- molten-nvim: https://github.com/benlubas/molten-nvim
- JupyterLab Desktop: https://github.com/jupyterlab/jupyterlab-desktop
- papermill remote engines: https://gateway-provisioners.readthedocs.io/en/latest/users/connecting-to-a-gateway.html, https://jupyter-enterprise-gateway.readthedocs.io/en/latest/developers/kernel-manager.html
- PyCharm/DataSpell remote servers: https://jetbrains.com/help/pycharm/configuring-jupyter-notebook.html
- Colab local runtime: https://github.com/googlecolab/jupyter_http_over_ws

## How nteract builds its UI

Read from `nteract/nteract` (last commit 2026-10-16), mainly `docs/adr/`.
Patterns worth considering for Ganymede's own UI (see [User interfaces](ui.md)):

- **One UI, several hosts.** A `NotebookHost` interface
  (`packages/notebook-host/src/{tauri,browser,electron}`) wraps every native
  call: dialogs, windows, logging, transport. Desktop, hosted cloud and a
  component gallery share one `NotebookDocumentShell`, fed a host-neutral
  capabilities object (read-only, ACLs). This maps onto a Swift/WKWebView
  host plus a browser host. (`docs/adr/notebook-host-shell-convergence.md`)
- **React doesn't own the document.** The document lives in a WASM peer
  (Automerge). An RxJS `SyncEngine` emits one stream per concern (cells,
  runtime state, outputs, presence), and a bridge writes them into small
  `useSyncExternalStore` stores read through hooks like `useCell(id)`. This
  stops kernel-status updates from re-rendering cell edits.
  (`docs/adr/frontend-sync-bridge.md`)
- **Local-first edits.** A keystroke mutates the local doc at once; the flush
  to the daemon is debounced, and execute/save wait for the flush explicitly.
- **Sandboxed outputs.** Rich outputs render in iframes with
  `sandbox="allow-scripts ..."` and no `allow-same-origin`, so each frame
  gets an opaque origin. Native bridges are never exposed to output frames.
  (`docs/adr/output-frame-security.md`)
- **Output lanes.** Plain streams render in the main DOM. Widgets and charts,
  document-like HTML, and large tables each get their own frame type, so a
  table can't trap page scrolling. (`docs/adr/output-rendering-segmentation.md`)
- **Stable DOM order.** Moving an iframe reloads it and loses widget state,
  so cells are never reordered in the DOM.
- **Stack.** React, CodeMirror 6, Radix/shadcn primitives, Tailwind with
  oklch semantic tokens, `ansi-to-react`. A `DESIGN.md` holds the token set.
