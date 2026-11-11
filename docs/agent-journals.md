# An agent and a person in one notebook

Coding agents are good at exploratory analysis: pull a data set, slice
it a few ways, join in other data, chart what changed. The work usually
happens in the agent's shell, and when it's done someone asks for a
write-up, which the agent reconstructs from memory. The code is cleaned
up after the fact and may not be what ran, outputs are rerun or
paraphrased, dead ends disappear, and nobody could watch or steer while
it happened.

With Ganymede the agent keeps the notebook as it works. Each step runs as
a cell on a real kernel and lands in a notebook you can watch live,
rerun from the top, and hand to someone else:

- Every result came from the cell above it, on the notebook's kernel,
  and the outputs saved in the file are the real ones.
- Dead ends stay, labelled, and a finding that turns out wrong is
  revised in place, with the old text kept in the cell's metadata.
- You can watch in JupyterLab and add cells of your own; the agent sees
  what you changed the next time it reads the notebook.
- The finished notebook reruns from the top, and opens in any Jupyter
  tool.

Two journals from sessions on cdp-kernel, on
[Seattle's Fremont Bridge bike counts](examples/journal-fremont.md) and
on [Oklahoma's earthquakes](examples/journal-oklahoma.md), are in the
[Examples](examples.md), with the prompts that steered them.

## How it works

![A document room. Clients such as JupyterLab tabs, a second person, nb-cli and ganymede mcp connect to ganymede serve over websockets and exchange yjs updates with one document room per open notebook. The room holds the notebook as a yrs document in jupyter_ydoc's schema, named for the file's id. It autosaves to the notebook file one second after the last edit, with If-Match on the hash it last read. The file stays the source of truth: other programs, such as an agent, git pull, a sync service or an editor, write it, and the room's watcher takes their changes in once they settle for half a second, at most five seconds after the first, merging them cell by cell.](images/rooms/overview.svg)

Agents and people edit the same live document. Three layers make that
work, each built on a protocol the Jupyter ecosystem already speaks:

- **Agent tools.** `ganymede mcp` is an MCP server over stdio. It joins
  a running `ganymede serve --mcp` whose root holds the current
  directory, so a person watching sees the agent's work, and otherwise
  starts a server in its own process. `serve --mcp` also serves the same
  tools over streamable HTTP. [Agents over MCP](mcp.md) has the setup and every tool;
  [nb-cli](agents.md), a notebook CLI for agents without MCP, works
  against the same server.
- **Document rooms.** Each open notebook is a yrs CRDT in the format
  JupyterLab's real-time collaboration uses, and the server runs cells
  into the room with jupyter-server-documents' execute API, so outputs
  land even when no client is connected.
  [Live editing and document rooms](rooms.md) has the details.
- **The file.** The `.ipynb` file stays the source of truth: the room
  saves to it a second after each edit, and merges in changes from git,
  a sync service or an editor cell by cell.

## Designed for how agents work

- **Journal by executing.** `cell_run` adds a cell and runs it in one
  call, so the record and the computation can't drift apart.
- **Scratch probes.** `record: false` runs code on the same kernel
  without adding a cell, so the agent can check a column name without
  cluttering the journal.
- **Honest about what ran where.** A shell download or a query run
  elsewhere becomes a Markdown cell marked `recorded`, never passed off
  as kernel output, and every agent cell carries
  [provenance](mcp.md#provenance): who wrote it, how, and when.
- **Revise in place.** `cell_edit` replaces a cell, and with `run: true`
  reruns it, keeping the replaced text in the cell's history.
- **A person in the loop.** `notebook_read` with `since` returns only
  what a person added or changed since the agent's last read.
- **Output shaped for models.** Results come back as compact text with
  terminal colours stripped and tracebacks trimmed; long output goes to
  a file, and SVG charts can be sent as images the agent can see.
- **Show results in the chat.** `cell_snapshot` turns cells into
  Markdown with charts saved as images.
- **Build on earlier work.** `notebooks_search` finds what earlier
  journals concluded about the same data.

## The journal plugin for Claude Code

One install bundles the MCP server with what makes a journal good: a
skill with the conventions (a question as the title, a finding after
each result, a clean run-all at the end), a reviewer agent that reads
the notebook cold and reruns it, and hooks that tell the agent what you
changed and stop it finishing while the journal has edits it hasn't
rerun.

```text
/plugin marketplace add tlockney/ganymede
/plugin install ganymede-journal@ganymede
```

Then ask for an analysis and say "keep a journal".
[The plugin](mcp.md#the-ganymede-journal-plugin-for-claude-code) has
each part, and how to watch live in JupyterLab.

## In the apps

The Mac, iPad and iPhone apps don't open notebooks through rooms yet,
so they don't show an agent's edits as they happen. Opening
notebooks through rooms, so the agent's journal shows live in the app,
is the [roadmap](roadmap.md)'s third priority.

## These docs, for an agent

[llms.txt](https://ganymede-server.com/llms.txt) indexes this site for
an agent: every page in sidebar order, with its first sentence, linking
the page's Markdown. Each page's Markdown is its URL plus `.md`, such as
[docs/install.md](https://ganymede-server.com/docs/install.md).
