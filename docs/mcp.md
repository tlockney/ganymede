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

## Tools

| Tool | Does |
| --- | --- |
| `notebook_open` | Open a notebook, creating it (and its directories) when missing, with an optional title cell; start or join its kernel. Later calls may leave out `path`. |
| `notebook_read` | The notebook as text: cells with ids, sources and outputs. A notebook in nbformat 4.4 or older has no cell ids on disk; the server makes them up when it opens the notebook, and they stay the same while the MCP session lasts. A session ends after an hour without a request, or when the server stops; after that, read the notebook again for new ids. `ganymede mcp` starts a new session by itself. A new session has no notebook open either, so open or read the notebook again, with its `path`, before calls that leave `path` out. |
| `cell_run` | Run code and return its outputs. Recorded as a new code cell by default; `record: false` runs it as scratch work that leaves nothing in the notebook but shares the kernel's state. |
| `cell_add` | Add a Markdown note; `mode: recorded` records a step that ran outside the kernel, such as a shell command. |
| `cell_edit` | Replace a cell's source, keeping the replaced text in the cell's history; `run: true` runs it after. |
| `cell_delete`, `cell_move` | Delete a cell, or move it after another. |
| `kernel_control` | Interrupt, restart, or shut down the kernel. |
| `notebook_run_all` | Restart the kernel and run every code cell in order. |
| `kernelspecs_list`, `notebooks_list` | The kernels available, and the notebooks in a directory. |

Each notebook is also a `notebook://<path>` resource, and the `journal`
prompt gives the agent conventions for keeping the record.

Cells run through the server, as the
[execute endpoint](protocol.md#document-model-and-collaboration) runs
them: outputs go into the document as they arrive, and a person watching
the notebook sees them live.

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
on the server's machine. An SVG over 2 MB, or one that can't be drawn,
is left out, as before.

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
