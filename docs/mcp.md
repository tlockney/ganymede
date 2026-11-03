# Agents over MCP

`ganymede mcp` gives a coding agent (Claude Code, Codex, Cursor, or any
other MCP client) tools to open, edit and run notebooks. It is meant for
journaling: the agent records an analysis in a notebook as it works,
code with its output and notes on what each step found, instead of
writing one up at the end. It needs no Python.

## Set up

Add it as a stdio MCP server. For Claude Code:

```sh
claude mcp add ganymede -- ganymede mcp
```

Other clients take the same command (`ganymede mcp`) in their MCP
settings. Paths in the tools are relative to the server's root.

`ganymede mcp` connects to a server in this order:

1. `--server URL` (with `--token` or `JUPYTER_TOKEN`).
2. A running `ganymede serve --mcp` whose root holds the current
   directory (or `--root-dir`), found by its
   [server info file](reference.md#server-info-file). Edits go through
   the server's document rooms, so they show live in JupyterLab (with
   `jupyter-collaboration`) or ganymede-ui while the agent works.
3. Otherwise, a server of its own in the same process, on a loopback
   port with a random token, rooted at the current directory. It stops,
   saving open notebooks, when the agent closes the connection.

Start a shared server with:

```sh
ganymede serve --mcp
```

`--mcp` (config key `mcp_enabled`) serves the same tools over
streamable HTTP at `/mcp`, behind the token, for clients that connect to
a URL instead of starting a command.

## The ganymede-journal plugin for Claude Code

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release. The plugin needs `ganymede mcp call` and read
> marks, which the latest release doesn't have: install
> [the latest main build](install.md#latest-main-build).

`ganymede-journal` packs the journaling mode into one Claude Code
plugin: the `ganymede mcp` server, a skill with the conventions, a
reviewer agent, and hooks that keep the record honest. Install it from
this repository's marketplace:

```text
/plugin marketplace add tlockney/ganymede
/plugin install ganymede-journal@ganymede
```

Then ask for an analysis and say "keep a journal", or start it by name
with `/ganymede-journal:journal`. The plugin runs `ganymede`, so it must
be on your `PATH`, and its hooks run with `node`.

| Part | What it does |
| --- | --- |
| MCP server `ganymede` | `ganymede mcp`, found or started as [above](#set-up). |
| Skill `journal` | The conventions: a title and goal, one question per section, scratch probes, recorded steps, revising findings in place, reading a person's cells, charts the agent can see, finishing with run-all and a summary. Its `typescript.md` covers cdp-kernel's display API, Polars and Observable Plot. |
| Agent `journal-reviewer` | Reads the notebook cold, re-runs it, and lists what a stranger couldn't follow or reproduce. It doesn't edit. |
| Hook `SessionStart` | Says whether a server is running for the directory, its kernels, and the journal open in a resumed session. |
| Hook `UserPromptSubmit` | Reads the open journal with the agent's [read mark](#changes-by-others) and adds what a person or another agent changed as context. |
| Hook `PostToolUse` | Follows the agent's notebook tools (the open notebook, its mark, edits since the last clean run-all). Records a Bash command that ends in `# journal` as a recorded cell, with up to 2,000 characters of its output. |
| Hook `Stop` | Once per round of edits, stops the agent from ending its turn while the journal has edits since its last clean `notebook_run_all`, and asks it to run it or say it is only pausing. |

The hooks reach the server with `ganymede mcp call` (below), so they
find the same server the agent uses: a running `ganymede serve --mcp`,
or the server the agent's `ganymede mcp` started. They keep a small
state file per Claude Code session in the plugin's data directory, and
stay silent when anything fails. Settings, as environment variables:

| Variable | Effect |
| --- | --- |
| `GANYMEDE_JOURNAL_RECORD` | `marked` (default): record Bash commands ending in `# journal`. `all`: every Bash command. `off`: none. |
| `GANYMEDE_JOURNAL_STOP` | `off` turns the `Stop` check off. |
| `GANYMEDE_BIN` | The `ganymede` binary, when it isn't on `PATH`. |

For the agent to see charts, start the server with images on:
`ganymede serve --mcp` with `mcp_images = true` in its config, or change
the plugin's server to `ganymede mcp --images` (which always starts a
server of its own, so a person can't watch live).

## Calling a tool from a script

`ganymede mcp call TOOL [ARGS]` calls one tool and prints its text, for
scripts and agent hooks. `ARGS` is a JSON object:

```sh
ganymede mcp call notebook_read '{"path": "journal/flake.ipynb", "since": "3f2a9c0d1b7e4a65"}'
```

It uses `--server`, else a running `ganymede serve --mcp` whose root
holds the current directory (or `--root-dir`), else the server an
agent's `ganymede mcp` started for itself there, which it announces in
`ganymede-mcp-<pid>.json` in the Jupyter runtime dir while it runs. It
starts no server: with none it fails. It exits 1 when the tool reports
an error, after printing the message. `--client NAME` sets the name
recorded in cells' provenance.

`cell_add` takes a `mark`: a read mark to move past the new cell, so a
hook that adds a cell for the agent (as the plugin's Bash hook does)
isn't reported to the agent as someone else's edit.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

## Tools

| Tool | Does |
| --- | --- |
| `notebook_open` | Open a notebook, creating it (and its directories) when missing, with an optional title cell; start or join its kernel. Later calls may leave out `path`. |
| `notebook_read` | The notebook as text: cells with ids, sources and outputs, then a read mark. With `since: <mark>` (unreleased), only what others changed since that read (see [Changes by others](#changes-by-others)). A notebook in nbformat 4.4 or older has no cell ids on disk; the server makes them up when it opens the notebook, and they stay the same while the MCP session lasts. A session ends after an hour without a request, or when the server stops; after that, read the notebook again for new ids. `ganymede mcp` starts a new session by itself. A new session has no notebook open either, so open or read the notebook again, with its `path`, before calls that leave `path` out. |
| `cell_run` | Run code and return its outputs. Recorded as a new code cell by default; `record: false` runs it as scratch work that leaves nothing in the notebook but shares the kernel's state. |
| `cell_add` | Add a Markdown note; `mode: recorded` records a step that ran outside the kernel, such as a shell command. |
| `cell_edit` | Replace a cell's source, keeping the replaced text in the cell's history; `run: true` runs it after. |
| `cell_delete`, `cell_move` | Delete a cell, or move it after another. |
| `kernel_control` | Interrupt, restart, or shut down the kernel. |
| `notebook_run_all` | Restart the kernel and run every code cell in order. |
| `kernelspecs_list`, `notebooks_list` | The kernels available, and the notebooks in a directory. |

Each notebook is also a `notebook://<path>` resource, and the `journal`
prompt gives the agent conventions for keeping the record.

`cell_run` waits up to `timeout` seconds (600 by default). A recorded
cell that runs longer keeps running, and its outputs keep arriving in
the notebook. A scratch run that runs longer is interrupted, since its
outputs go nowhere and later runs on the shared kernel would wait
behind it. A scratch run that times out before the kernel started it
(queued behind a recorded cell still running, or a cell run from
JupyterLab) interrupts nothing, so that other run isn't stopped; the
reply says the code runs when the kernel is free.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

Cells run through the server, as the
[execute endpoint](protocol.md#document-model-and-collaboration) runs
them: outputs go into the document as they arrive, and a person watching
the notebook sees them live.

## Changes by others

A person may edit the notebook in JupyterLab or ganymede-ui while the
agent works. `notebook_open` and `notebook_read` end with a read mark:

```text
@@read 3f2a9c0d1b7e4a65
```

`notebook_read` with `since: "3f2a9c0d1b7e4a65"` (and `path`, or the
mark's own notebook when left out) returns only the
cells others added, edited or moved since that read, each as an
`@@cell` block whose head has `"change": ["added"]`, `["edited"]`,
`["moved"]` or more than one, and an `@@deleted {"id": ...}` line per
deleted cell. With no changes it says so. Cell sources and types are
compared, not outputs or metadata. A cell is moved when it left the
longest run of cells that kept their order.

Each MCP connection has one mark per notebook, and it keeps its name.
Reading the notebook moves the mark to what was read, and the
connection's own edits (`cell_add`, `cell_run`, `cell_edit`,
`cell_delete`, `cell_move`) move it too, so they are not reported
back. An edit moves only what it changed (`cell_edit` the source,
`cell_move` the place), and a failed edit moves nothing, so someone
else's change to the same cell is still reported. Reading with a mark
makes it the reading connection's mark for that notebook, so after
`ganymede mcp` starts a new session the agent's mark keeps following
its edits. Another connection to the same server can read with the mark, as
a hook outside the agent would; that read moves the mark as well, so
the agent isn't told the same change twice.

Marks are kept in the server's memory. After a restart, or once more
than 4,096 marks are in use, a mark may be unknown: the read then says
so and returns the whole notebook with a mark to use next. A mark names
one notebook path, so after a rename the old mark is treated the same
way.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

## What the agent sees

Outputs come back as text in nb-cli's `@@cell` / `@@output` format:
Markdown or plain text, HTML tags stripped, tracebacks without terminal
colors and cut to the last frames. An output over 4,000 characters is
written to a file named by its SHA-256 hash in `ganymede-mcp-outputs`
in the Jupyter runtime dir (readable only by you), and the reply gives
its path. Images are left out unless the agent's
server was started with `--images`, which sends them as MCP image
content. Kernels can't ask for input.

With `--images`, an SVG output (what Observable Plot, Vega-Lite and
most chart libraries draw) that has no PNG or JPEG of its own goes to
the agent as a PNG, white background, at most 1,600 pixels on its
longer side. The notebook keeps the SVG. Text uses the fonts installed
on the server's machine. Images inside the SVG are drawn only from
`data:` URLs; the server reads no files an SVG names. An SVG over 2 MB,
or one that can't be drawn, is left out, as before.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

## Provenance

Cells an agent writes carry
`metadata.ganymede.agent = {"by": "<client name>", "mode": "executed" | "recorded" | "note", "at": "<time>"}`,
where the name is the one the MCP client gave when it connected. Scratch
runs leave no cells and run with `store_history: false`, so a kernel
that honours it, as ipykernel and cdp-kernel do, doesn't advance its
execution count. They share the kernel's state: a variable a scratch run
assigns stays defined. Other history is kernel-dependent. With
ipykernel, a scratch result still sets `_`, and `Out[N]`/`_N` for the
next recorded cell's number N, which keeps the scratch value if that
cell has no result. A function a scratch run defines shows as
`Cell In[N]` in tracebacks.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

When `cell_edit` changes a cell's source, the source it replaced is kept
in `metadata.ganymede.agent.history`, oldest first, as
`{"source": "<old text>", "by": "<client name>", "at": "<time>"}`: the
replaced text, who replaced it and when. A cell keeps the last 10; an
edit that leaves the source as it was adds none. So a finding the agent
revised in place still shows what it said before, and a UI can show the
revisions. The replaced text is saved in the `.ipynb`: editing a secret
out of a cell with `cell_edit` leaves it in `history` until the metadata
is cleared.

## Options

These apply to a server `ganymede mcp` starts itself, so each makes it
start one even when a shared server is running. For `serve --mcp` they
are config keys.

| Flag | Config key | Effect |
| --- | --- | --- |
| `--read-only` | `mcp_read_only` | Read notebooks only: no edits, no execution. |
| `--no-execute` | `mcp_no_execute` | Edit notebooks but run no code. |
| `--images` | `mcp_images` | Send images in outputs to the model. |
| `--own-server` | | Start a server even when one is running. |
| `--default-kernel` | | The kernelspec new notebooks start. |

## nb-cli

[nb-cli](clients.md#agent-clis) is another way in: a command-line tool
for agents that talks to `ganymede serve` over the Jupyter API. It
fits agents without MCP, or a person who wants the same commands in a
terminal. [Agents](agents.md) sets it up with Claude Code and covers the
journaling conventions, which apply to the MCP tools as well.
