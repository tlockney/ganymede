# A host API for kernels

A design note for [#647](https://github.com/tlockney/ganymede/issues/647):
a way for cell code to ask the Ganymede server for things, such as
reading, searching and running other notebooks, without giving the
kernel a server token. It is a proposal for review. Nothing here is
built yet.

## The problem

A cell can ask the app for things through the
[`device` API](https://github.com/tlockney/ganymede-device). It can't
ask the server for anything. Since
[#627](https://github.com/tlockney/ganymede/pull/627) a kernel learns its
notebook's path from `JPY_SESSION_NAME`, and that is all it learns. It has
no server URL and no token.

That blocks the notebooks-from-notebooks use cases: a journal index that
lists every journal, "what do I already know about X" searched from a
cell, a new journal that reads an older one's named results, and a
notebook that re-runs another one with new parameters. On a desktop, a
Node or Deno cell can read files itself, but it can't see a notebook
that is ahead of its file in a [room](rooms.md), can't run a notebook in
a fresh kernel, and has no search. A kernel in the iPad and iPhone app
has no file system at all.

The obvious fix, passing the server's URL and token to the kernel, gives
every cell the whole API: terminals, other kernels, every file under the
root. An agent's cell would get the same. This note proposes a narrower
channel instead.

| What a cell needs | How it travels | Here? |
| --- | --- | --- |
| Which notebook it is | Environment at launch (`JPY_SESSION_NAME`) | Already on `main` |
| Run parameters | Data at launch (the run contract) | No, see [The run contract](#the-run-contract) |
| Named results | An output with a MIME type | No, see [The run contract](#the-run-contract) |
| Other notebooks: list, read, search, results, run | A live call to the server | Yes |
| Secrets | A live call to the server | Later, same channel |
| Device features | A live call to the app (`device`) | No, built |

The first three need no new protocol: data goes in at launch and comes
back in outputs. The host API is for what needs a live answer.

## The proposal

The server opens a comm to each kernel it starts, and answers calls on
it under a namespace, `host`, beside `device`. The app answers `device`;
the server answers `host`. The kernel never holds a token, and what each
kernel may ask is granted per notebook, on the server.

![The parts of a host call. In the kernel, cell code calls jupyter.host, a thin client. It sends each call as a comm message on the host comm, which the server opened when the kernel started. In the Ganymede server, the responder takes the call, checks it against the notebook's grant, and calls the notebooks core, which reads files and rooms and runs notebooks. The MCP tools use the same core and render its answers as text for a model. The reply goes back to the kernel on the shell channel, on a kernel subshell, so a waiting cell gets it. The server keeps the host comm's messages out of what it forwards to other clients, such as JupyterLab or the apps.](images/host-api/overview.svg)

In a cell, with cdp-kernel:

```ts
const host = await jupyter.host();             // null when no server answers
host.notebook;                                 // "journal/2026-11-11-quakes.ipynb"
host.has("notebooks.run");                     // false unless granted

const journals = await host.notebooks.list({ dir: "journal" });
const hits = await host.notebooks.search({ query: "weekday drop", dir: "journal", outputs: true });
const nb = await host.notebooks.read({ path: hits[0].path });  // nbformat JSON
const results = await host.notebooks.results({ path: "journal/2026-11-09-fremont-bridge.ipynb" });
results.weekday_drop;                          // -0.19

const run = await host.notebooks.run(
  { path: "journal/2026-11-09-fremont-bridge.ipynb", params: { counter: "2nd Ave" } },
  { signal: jupyter.signal },                  // an interrupt stops the run
);
run.status; run.results;
```

A read-and-query package for notebooks (the notebooks-from-notebooks
research's level 1) sits on top of these calls: journal views, findings,
named results. Where that package lives is still open; it isn't part of
this note.

### Transport: a comm the server opens

When a kernel's first `kernel_info_reply` arrives, the server sends a
`comm_open` on shell to target `ganymede.host.v1`, with its `hello`
reply as the open's data. After a restart it opens a new one.

- **A kernel without the client loses nothing.** A kernel with no
  `ganymede.host.v1` target answers the open with `comm_close`, as
  cdp-kernel and ipykernel both do, and the server stops there. Cells
  that never call `host` see no difference.
- **Calls** go from the kernel to the server as `comm_msg` on iopub:
  `{ id, method, params }`, and `{ id, cancel: true }` when the cell
  aborts. This is the `device` comm's message format, so the client code
  is shared.
- **Replies** go from the server to the kernel as `comm_msg` on shell:
  `{ id, result }` or `{ id, error: { code, message } }`, with the
  `device` error codes (`unavailable`, `denied`, `cancelled`, `timeout`,
  `failed`, `disconnected`). Shell isn't broadcast, so no other client
  sees a reply.
- **Subshells.** A cell that awaits a call is still running, so a reply
  on the main shell would wait behind it forever. The server asks for a
  kernel subshell (`create_subshell_request` on control) before opening
  the comm, and sends the open and every reply on it. cdp-kernel has
  subshells since 0.8.0. A kernel that lists no `kernel subshells` in
  `supported_features` gets no host comm, since every awaited call on it
  would hang.
- **Kept from other clients.** The server forwards a kernel's iopub to
  every connected client. It drops from that stream the host comm's
  `comm_msg` and `comm_close`, and the `status` messages whose parent is
  one of its own host messages. JupyterLab and the apps never see the
  comm, so the `device` problem (a client closing a comm whose target it
  doesn't know) can't arise, and a call's parameters stay between the
  kernel and the server. A process that reads the kernel's ZeroMQ iopub
  socket directly, with the connection file, still sees calls, as it
  sees every output.
- **No pings.** The `device` comm pings because a frontend can vanish
  without closing. The server is the kernel's host and learns of a
  kernel's death directly, so a dead kernel's calls end with it.

![A host call while a cell runs. The server opened the host comm on a kernel subshell when the kernel started. The cell calls host.notebooks.search, and the kernel sends a comm_msg with the call's id, method and params on iopub. The server keeps it out of what other clients receive, checks the notebook's grant, runs the search through the notebooks core, and replies with a comm_msg carrying the id and result on shell, on the subshell. The kernel reads the subshell while the cell runs, and the cell's await resolves. Other clients see the cell's outputs as usual, but not the host messages.](images/host-api/call.svg)

#### A client that loads late

cdp-kernel registers `ganymede.host.v1` when it starts, so it is ready
for the open. A client that loads later, such as a Python package a cell
imports, has missed it. Such a client opens the comm itself, to the same
target, with no data. The server takes a kernel-opened comm to
`ganymede.host.v1`, keeps it from other clients like any host message,
answers with `hello` over it, and then treats it as the host comm. One
kernel has one host comm: a second open is closed with `{ error:
"taken" }`.

#### Which server answers

The server that launched the kernel answers. In the apps that is the
embedded server, and a kernel registered through the C ABI
([In-app kernels as server kernels](in-app-kernel.md)) gets the comm the
same way, so the in-app kernel needs no transport of its own. A kernel
behind a [kernel server](kernels.md) (`gateway_url`) is launched by the
remote server, so the local one opens no host comm for it. If the remote
is Ganymede, it answers with its own grants.

### The calls

Every call takes a JSON object and resolves with JSON. Paths are API
paths, relative to the server's root, as in the contents API. The first
version has these:

| Method | Params | Result | Granted by default |
| --- | --- | --- | --- |
| `hello` | `{}` | `{ protocol: 1, server, notebook, methods }`: the server's version, this kernel's notebook path (or `null`) and the methods it may call | Always |
| `notebooks.list` | `{ dir? }` | `[{ path, title, kernel, modified }]` for notebooks and directories in `dir` | Yes, under `read` |
| `notebooks.read` | `{ path }` | The notebook as nbformat 4 JSON, from its room when one is open, so it includes what isn't saved yet | Yes, under `read` |
| `notebooks.search` | `{ query, dir?, outputs?, cell_type?, limit? }` | `{ hits: [{ path, title, index, id, cell_type, in, excerpt }], total, truncated }`, the `notebooks_search` tool's matches as data | Yes, under `read` |
| `notebooks.results` | `{ path }` | `{ name: value }`: the notebook's named results, from its saved outputs | Yes, under `read` |
| `notebooks.run` | `{ path, params?, save?, timeout? }` | `{ status, results, error?, path? }` | No |

`has(method)` is answered in the kernel from `hello`'s `methods`, so a
cell can check before it calls.

Later, on the same channel and the same grants:

- `notebooks.edit`: cell add, edit, delete and move through the room,
  with the execute endpoint's source-hash guard, as the MCP cell tools
  do. Not needed by the notebooks-from-notebooks use cases yet.
- `secrets.get`: a named secret the notebook is granted, from a store
  the server keeps. Its reply is the reason replies stay on shell.
- `runs.list`: the run ledger, once it exists.

#### `notebooks.run`

Runs another notebook in a fresh kernel, with parameters, and returns
its status and named results. It is one front door to
[the run contract](#the-run-contract), beside `ganymede execute -p` and an
HTTP endpoint, so it waits on that contract's first step.

- The notebook's own kernelspec runs it. A TypeScript notebook can run a
  Python one if the server has a Python kernel.
- `save` is `false` by default: the run's outputs go nowhere but the
  result. `"copy"` writes the executed notebook beside the run contract's
  other copies and returns its `path`. Writing back into the notebook
  itself is not offered from a cell.
- An abort (`{ signal: jupyter.signal }`, which an interrupt aborts)
  sends `cancel`, and the server shuts the run's kernel down.
- **Grants narrow, never widen.** The run's kernel gets the intersection
  of the called notebook's grant and the caller's, so a notebook can't
  reach more by running another one.
- **Depth and width.** A run started from a run counts one deeper, and
  the server refuses past depth 3 (`denied`). Each notebook has at most
  two runs of its own in flight; more wait their turn.

#### Errors and trust

- A call that isn't granted fails `denied` and names the grant it needs,
  so a cell can say what to ask for. A method the server doesn't have
  fails `unavailable`.
- What comes back from another notebook is data written by whoever ran
  that notebook, possibly an agent reading a web page. A cell, or an
  agent reading its output, should treat it as untrusted input. The
  host API doesn't mark it trusted, and named results are JSON, never
  HTML.

### Grants

A grant says what a notebook's kernel may ask the server for. The
server keeps grants; a notebook can't grant itself.

```toml
# ganymede.toml: defaults for every notebook
[host]
read = [""]          # API paths the notebooks.* reads may reach; "" is the root
run = []             # notebooks this notebook may run; [] is none, [""] is all
```

- **Defaults** come from `[host]` in the config file. Out of the box a
  notebook may list, read, search and read results of notebooks under
  the root, and may run none. A Node or Deno kernel can read those files
  already, and an in-app kernel needs reads to do anything.
- **Per notebook** grants add to or narrow the defaults, keyed by the
  notebook's [file id](rooms.md#file-ids), so they follow a rename or
  move. They live beside the file id database, not in the notebook.
- **Asking.** A notebook may list what it wants in
  `metadata.ganymede.host.wants`, for example `["notebooks.run"]`. That
  grants nothing. A client shows it and lets the person grant it, the way
  Trust Notebook works for outputs. A downloaded notebook that asks for
  everything gets nothing until someone says yes.
- **Granting** is `ganymede grant <notebook> run=<path>` on the command
  line first, then a sheet in the apps. A notebook with no file (an
  untitled console) gets the defaults.
- **`ganymede execute`** has no file id database. It uses `[host]` from
  the config and `--grant run=<path>` on the command line.

This is the server's half of "scoped kernels" from the direction review:
it limits what a kernel can ask the server for. It doesn't limit what a
kernel's own code can do on the machine. That is the OS sandbox around
the kernel process, a separate step that can read the same grants.

### One implementation, two doors

The MCP tools already read, search and run notebooks, but their answers
are text for a model, in nb-cli's `@@cell` format. The host API needs
the same operations returning data. So the work splits the tools in two:

- A **notebooks core** that returns data: list, read (room or file),
  search, results, and later run and edit. `mcp/search.rs` already works
  this way, returning hits as structs.
- The **MCP tools** render the core's answers as text, as today.
- The **host responder** checks the grant and returns the core's answers
  as JSON.

The comm driver (open on start, the subshell, routing replies, the
client fan-out filter) and the file-based core go in a new crate,
`ganymede-host`, that `ganymede-engine` uses. `ganymede execute` and an
app that embeds the engine then answer `host` calls with no HTTP server.
`ganymede-server` adds what needs a server: reads from open rooms, and
the per-notebook grants. The crates under it stay language-free: the
server sees method names and JSON.

### The run contract

Theme A of the direction review gives a run three parts: parameters
passed as data at launch, named results published as outputs, and one
way to run a notebook. `notebooks.run` and `notebooks.results` depend on
the last two.

Named results get a MIME type of Ganymede's own, with a name and a JSON
value, for example `application/vnd.ganymede.result+json` holding
`{ "name": "weekday_drop", "value": -0.19 }`, which cdp-kernel's
`jupyter.result()` writes. `notebooks.results` also reads scrapbook's
`application/scrapbook.scrap.json+json` outputs, so Python notebooks
written with scrapbook's `glue` work as they are. When a name appears
more than once, the last output wins, as in scrapbook. The exact shape
belongs to the run contract's own change; this note relies only on
results being readable from saved outputs by name.

## What changes in each repository

- **ganymede**: `ganymede-host` (comm driver, file-based notebooks core,
  `hello`, the `notebooks.*` methods), the iopub filter in the kernel
  manager, the MCP tools moved onto the core, grants in the server and
  the config file, `ganymede grant`, and a page in the docs. The protocol
  page records the `ganymede.host.v1` target and its messages.
- **cdp-kernel**: `jupyter.host()`, a client that registers
  `ganymede.host.v1` at start, waits for the open, and calls with `{
  signal }`. It shares the call and error code with `ganymede-device`'s
  comm transport.
- **ganymede-ui**: nothing for the first step. Later, the sheet that shows
  a notebook's `wants` and grants them, and `has()` badges.
- **ganymede-device**: none. `device` keeps its transports, since the app
  answers it, not the server.

The order, once this note is agreed: the comm and the read methods in
Ganymede, then `jupyter.host()` in cdp-kernel, which is enough for the
journal index and "what do I already know". `notebooks.run` follows the
run contract's first step. Granting in the apps comes last.

## Why a comm and not a token

The alternative was to pass `GANYMEDE_SERVER_URL` and a token scoped to
notebook calls in the kernel's environment, with the calls as HTTP
endpoints.

- A token in the environment leaks into child processes, logs and
  outputs (`console.log(process.env)`), and a cell can send it anywhere.
  The comm gives cell code nothing to copy: only calls on a channel the
  server checks.
- A scoped token is a second authorization system beside the server's
  one token. Grants checked by the responder are one place.
- The in-app kernel and a kernel on WASI have no network route to the
  server. A comm travels on the kernel's own channels wherever it runs,
  including through the C ABI.
- It adds no network surface: no new endpoint, and no listener the
  kernel dials.

What it costs: it needs subshells to answer a waiting cell, so kernels
without them get no host API, and each language needs a small client.
That client is a comm target and a request map, about the size of
`ganymede-device`'s comm transport.

## Open questions

- **Default reads.** The proposal lets every notebook read the whole
  root by default, which matches what a Node or Deno kernel can already
  do. A stricter default (only the notebook's own directory) would make
  in-app and agent-run notebooks safer, at the cost of a grant for the
  journal index.
- **Where grants live.** Keyed by file id beside the file id database,
  as proposed, or in a file people can read and edit, such as a
  `[host.notebooks]` table in `ganymede.toml` keyed by path.
- **Kernels without subshells.** ipykernel 7 has them; older Python
  kernels and other languages may not. Reads could still work for a
  client that doesn't await on the main shell (a callback API), but
  that is a second way to call. The proposal leaves those kernels out.
- **A cell's own notebook.** Writing to its own notebook (regenerating
  an index table in a Markdown cell) is the most-asked edit. It could
  come before general `notebooks.edit`, granted by default for the
  notebook itself.

## What this leaves out

Secrets and their store, the OS sandbox, the run ledger, edits through
the host API, and where the notebooks read-and-query package lives. Each
builds on this channel and these grants.
