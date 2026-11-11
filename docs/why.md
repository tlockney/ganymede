# Why Ganymede

Ganymede started as a way to write TypeScript notebooks and run them on
an iPad. It became a notebook runtime with a Rust core: the Mac, iPad and
iPhone apps are the main product, and the same core runs as a command, a
server, or behind an agent. Its author builds it for their own use
first. Others may find it useful, but it is not trying to win users.

## Where it started

The first design, [ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados),
was an iPad client in front of kernels on a home server over a tailnet,
then a kernel on the iPad itself. Two things followed from it:

- **Keep the Jupyter protocols.** The messaging protocol, the REST and
  websocket API, and nbformat are language-neutral and well specified.
  Keeping them keeps JupyterLab, VS Code, the existing kernels, and
  every `.ipynb` file.
- **Replace the server.** The reference server, `jupyter_server`, is a
  Python program, and Python doesn't travel to an iPad app. A Rust core
  does: as a binary, as a library linked into an app, and to wasm.

[cdp-kernel](https://cdp-kernel.org), a TypeScript kernel extracted from
Deno's, grew alongside it, and is the kernel Ganymede is built around.
[The iPad use case](ipados.md) has the original plan and what it
decided.

## What it is for now

Four priorities, in order:

1. **The notebook as a program.** A notebook takes parameters, returns
   named results, and runs from the CLI, the server API, app links,
   Shortcuts and other notebooks. A kernel can read and run notebooks
   through the host API, under explicit per-notebook grants; a notebook
   run from another can't reach more than its caller. ([Notebooks as programs](programs.md))
2. **The apps as a full runtime.** The apps run notebooks in-process
   with no external server, with device APIs, automation, widgets and
   the share extension, and can switch to a remote server from the
   window. ([Ganymede's app](app.md))
3. **A person and an agent in one live notebook.** An agent reads,
   edits and runs the same live document a person has open.
   ([An agent and a person in one notebook](agent-journals.md))
4. **TypeScript notebooks for TypeScript developers**, with cdp-kernel
   as the first-class kernel.

Not now: Python in the apps through Pyodide, MCP Apps, a native kernel
for another language, CurveZMQ. The [Roadmap](roadmap.md) has what comes
next under each priority.

## Principles

- **Only the core is Rust.** The protocol, kernel host, notebook format,
  contents, API and server. User interfaces, ganymede-ui and the apps'
  Swift shell included, are clients of the API.
- **The host knows no language.** The host crates name no language
  outside their tests; kernels come from kernelspecs, and parameters
  and results are JSON and a MIME type.
- **The Jupyter API is a compatibility target.** JupyterLab, VS Code
  and Jupyter's tools should work unchanged; Ganymede adds routes beside
  the standard ones, not in place of them.
- **The file is the source of truth.** Saves are atomic and match
  Python's nbformat byte for byte; live rooms merge outside changes in
  cell by cell.
- **Conservative defaults.** A random token, loopback only, crash
  reporting off. Grants, permissions and "always allow" are scoped as
  narrowly as they can be.

## How it is built

Thomas Lockney sets the direction and decides what ships. Most of the
code and docs are written by Claude sessions working from GitHub
issues: every change is an issue, a pull request with green CI, and an
entry in the [development log](notes.md). Each pull request is also
reviewed by a separate Claude session that didn't write it, against its
issue and the project's rules. Releases go out only on Thomas's word
([Releasing](releasing.md)).

The development log is the long record: what each step found, what
failed, and why a choice was made.

## The arc

| Release | Date | What it added |
| --- | --- | --- |
| [0.8.0](https://github.com/tlockney/ganymede/releases/tag/v0.8.0) | 2026-11-11 | Parameters and named results (`execute -p --results`, `POST /api/notebooks/run`); the host API with per-notebook grants; the apps' own kernel registered with the server |
| [0.7.0](https://github.com/tlockney/ganymede/releases/tag/v0.7.0) | 2026-11-06 | One owner per notebook room on a machine; journaling evals; the apps on the landing page |
| [0.6.0](https://github.com/tlockney/ganymede/releases/tag/v0.6.0) | 2026-11-06 | The ganymede-journal plugin for Claude Code; opt-in crash reporting; kernel logs, memory and CPU |
| [0.5.0](https://github.com/tlockney/ganymede/releases/tag/v0.5.0) | 2026-11-03 | Document rooms and the execute endpoint; `ganymede mcp`; terminals; language servers for jupyterlab-lsp |
| [0.4.0](https://github.com/tlockney/ganymede/releases/tag/v0.4.0) | 2026-10-20 | A container image; the embedding library; the guide to Ganymede's app |
| [0.3.0](https://github.com/tlockney/ganymede/releases/tag/v0.3.0), [0.2.0](https://github.com/tlockney/ganymede/releases/tag/v0.2.0) | 2026-10-18 | Hardening for daily use: save preconditions and per-path locks, kernel shutdown and signals, auth and REST parity fixes, release provenance, SECURITY.md |
| [0.1.0](https://github.com/tlockney/ganymede/releases/tag/v0.1.0) | 2026-10-17 | The server: kernel host, notebook format, executor, contents and the Jupyter REST and websocket API |

The [changelog](changelog.md) lists every change.
