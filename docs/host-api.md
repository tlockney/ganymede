# A host API for kernels

A design note for [#647](https://github.com/tlockney/ganymede/issues/647):
a way for cell code to ask the Ganymede server for things, such as
reading, searching and running other notebooks, without giving the
kernel a server token. The design was agreed on 2026-11-10, with the
decisions at the end. The first step is built; [What's built](#what-s-built)
says which.

## What's built

The server opens the host comm to every kernel with subshells, keeps its
traffic from other clients, and answers `hello`, `notebooks.list`,
`notebooks.read`, `notebooks.search` and `notebooks.results`, with the `[host] read` grant
from the config file ([Reference](reference.md#the-host-api)), and
`self.add`, `self.edit` and `self.delete`, unless `[host] edit_self` is
off, and `notebooks.run` under the `[host] run` grant (none by
default). A notebook's own grant, set with `ganymede grant`, replaces
those defaults for it, and clients set it over HTTP with
`/api/host/grants`. cdp-kernel's client is `jupyter.host()`. In the
apps, Kernel > Notebook Access… (the ⋯ menu on the iPad and iPhone) shows
what a notebook asks for and sets its grant.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

`ganymede execute` answers its kernels' calls as well, under `[host]`
with `--grant read=PATH` or `run=PATH` in place of a key
([Reference](reference.md#under-ganymede-execute)).

`notebooks.run` (and `POST /api/notebooks/run`) take `save: "copy"`,
which keeps the executed notebook under `runs/`
([Run copies](reference.md#run-copies)).

The rest of this page is the design.

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
const { notebook } = await host.notebooks.read({ path: hits[0].path });  // nbformat JSON
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
| `notebooks.read` | `{ path }` | `{ notebook, hashes }`: the notebook as nbformat 4 JSON, from its room when one is open, so it includes what isn't saved yet, and each cell's source hash by cell id | Yes, under `read` |
| `notebooks.search` | `{ query, dir?, outputs?, cell_type?, limit? }` | `{ hits: [{ path, title, index, id, cell_type, in, excerpt }], total, truncated }`, the `notebooks_search` tool's matches as data | Yes, under `read` |
| `notebooks.results` | `{ path }` | `{ name: value }`: the notebook's named results, from its outputs, read from its room when one is open, like `notebooks.read` | Yes, under `read` |
| `notebooks.run` | `{ path, params?, save?, timeout? }` | `{ status, results, error?, path?, copy? }` | No |
| `self.add` | `{ cell_type, source, after? }` | `{ id }` of the new cell in the calling notebook | Yes |
| `self.edit` | `{ id, source, source_hash }` | `{}`, or `failed` with `source_mismatch` when the cell changed since it was read | Yes |
| `self.delete` | `{ id, source_hash }` | `{}`, with the same check | Yes |

`has(method)` is answered in the kernel from `hello`'s `methods`, so a
cell can check before it calls.

The `self.*` calls edit only the notebook the kernel was started for,
through its room, so every client sees the change live. They use the
MCP cell tools' code and the execute endpoint's source hash (MurmurHash2,
seed 0), so a cell can't overwrite an edit it hasn't seen. The hash
comes from `notebooks.read`, which returns each cell's beside the
notebook, so a client needn't compute it. A notebook may always read
itself, whatever its `read` grant, so a narrowed grant can't leave
`self.*` without hashes. They cover
the most common edit a notebook makes, regenerating a table or summary
in one of its own cells, such as a journal index. A kernel with no
notebook has no `self.*` calls.

Later, on the same channel and the same grants:

- `notebooks.edit`: the `self.*` calls for other notebooks, behind a
  grant. Not needed by the notebooks-from-notebooks use cases yet.
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
  result. `"copy"` writes the executed notebook under `runs/` and
  returns its path as `copy`
  ([Run copies](reference.md#run-copies)), only when the calling
  notebook may read the notebook it runs as well as run it. Writing back into the
  notebook itself is not offered from a cell.
- An abort (`{ signal: jupyter.signal }`, which an interrupt aborts)
  sends `cancel`, and the server shuts the run's kernel down.
- **A run never widens its caller's grant.** The run's kernel gets the intersection
  of the called notebook's grant and the caller's, so a notebook can't
  reach more by running another one. A path must pass each grant up the
  chain with symlinks resolved, so a grant that names a symlink reaches
  nothing its caller can't.
- **Depth and width.** A run started from a run counts one deeper, and
  the server refuses past depth 3 (`denied`). Each kernel has at most
  two runs of its own in flight; more wait their turn. The limit counts
  per calling kernel, not per notebook, so a notebook that runs itself
  can't wait on a slot its own caller holds: every run in a chain has
  its own kernel and its own two slots.

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
edit_self = true     # the self.* calls; false turns them off
```

- **Defaults** come from `[host]` in the config file. Out of the box a
  notebook may list, read, search and read results of notebooks under
  the root, may edit its own cells (`self.*`), and may run none. A Node or Deno kernel can read those files
  already, and an in-app kernel needs reads to do anything.
- **Which notebook.** A kernel gets the grant of the notebook whose
  session the server started it for: that session's API path, mapped to
  its file id, which `hello` reports as `notebook`. It is the notebook
  whose OS path goes into `JPY_SESSION_NAME`, but the server reads its
  own session record, the first it sees for the kernel, not the
  variable. Another session that joins the same kernel (a
  console attached to it) adds nothing. A kernel started with no
  notebook gets the defaults, and `notebook` is `null`.
- **Paths** in calls and grants are normalized as the contents API
  normalizes them: relative to the root, with no `..` and no way out of
  the root through a symlink. A grant's path matches whole segments, so
  `journal` covers `journal/x.ipynb` but not `journal-private/`, and is
  matched against where a path leads once symlinks are resolved, so a
  symlink in a granted folder doesn't reach another.
- **Per notebook** grants add to or narrow the defaults, keyed by the
  notebook's [file id](rooms.md#file-ids), so they follow a rename or
  move. They live beside the file id database, not in the notebook.
- **Asking.** A notebook may list what it wants in
  `metadata.ganymede.host.wants`, for example `["notebooks.run"]`. That
  grants nothing. A client shows it and lets the person grant it, the way
  Trust Notebook works for outputs. A downloaded notebook that asks for
  everything gets nothing until someone says yes.
- **Granting** is `ganymede grant <notebook> run=<path>` on the command
  line first, with `--list` to show a notebook's grants (or every
  notebook's) and `--revoke` to remove one, then a sheet in the apps. A notebook with no file (an
  untitled console) gets the defaults.
- **`ganymede execute`** uses `[host]` from the config with
  `--grant read=<path>` or `--grant run=<path>` in place of a key as the
  defaults, and a notebook's own grant over them, as a server does. It
  gives its kernels no `self.*` calls, since it writes the notebook
  itself when the run ends.

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
`ganymede-host`, with two users. `ganymede-api`'s kernel manager opens
the comm for the server's kernels and filters host traffic in its iopub
fan-out (`Clients::dispatch` in `bridge.rs`). `ganymede execute`
answers with the server's own responder and no listener
(`serve::LocalHost`), so the grant checks and the runs are the same
code.
`ganymede-server` adds what needs a server: reads from open rooms, and
the per-notebook grants. The crates under it stay language-free: the
server sees method names and JSON.

### The run contract

Theme A of the direction review gives a run three parts: parameters
passed as data at launch, named results published as outputs, and one
way to run a notebook. `notebooks.run` and `notebooks.results` depend on
the last two.

The run contract's first step
([#650](https://github.com/tlockney/ganymede/issues/650)) passes
parameters as a JSON object in `GANYMEDE_PARAMS`, which cdp-kernel
exposes as `jupyter.params`. Named results are `display_data` outputs
with the MIME type `application/vnd.ganymede.result+json`, holding `{
"name": "weekday_drop", "value": -0.19 }`, which cdp-kernel's
`jupyter.result()` writes. Scrapbook's outputs (`application/scrapbook.scrap.json+json` and
`application/scrapbook.scrap.text+json`) are read too, so Python notebooks written with scrapbook's `glue`
work as they are, and when a name appears more than once the last output
wins. A language-free reader in `ganymede-nbformat` collects them, and
`notebooks.results` and `notebooks.run` use it, so the host API adds no
channel of its own for parameters or results.

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

The order: the comm and the read methods in Ganymede, then
`jupyter.host()` in cdp-kernel, which is enough for "what do I already
know". The `self.*` calls come next, which complete the journal index.
`notebooks.run` follows the run contract's first step. Granting in the
apps comes last.

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

## Decisions

Thomas settled the four open questions on 2026-11-10:

- **Default reads cover the whole root.** A desktop kernel can read
  those files already, so a narrower default would add a grant without
  protection, and in the apps the root is the app's own documents.
  Limiting what an agent's code can do on the machine is the OS
  sandbox's job.
- **Grants are kept by file id**, so they follow a notebook that moves.
  `ganymede grant --list` and `--revoke` show and remove them, in place
  of editing a file by hand.
- **Kernels without subshells get no host API.** cdp-kernel and
  ipykernel 7 have subshells. A second, callback-style way to call
  waits until a kernel that needs it comes up.
- **A notebook may edit itself** through the `self.*` calls, granted by
  default, as the step after the read calls. Editing other notebooks
  stays for later.

## What this leaves out

Secrets and their store, the OS sandbox, the run ledger, edits to other
notebooks, and where the notebooks read-and-query package lives. Each
builds on this channel and these grants.
