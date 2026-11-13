# An agent that responds to your edits

A design note for [#761](https://github.com/tlockney/ganymede/issues/761):
when a person edits a journal the agent is working in, how the agent
learns of it, who it learns made the change, and when it acts on it.
Thomas agreed the four recommendations on 2026-11-13
([Decisions](#decisions)), and asked for waking on any edit (1b) to be
tried as a spike once request cells work. Nothing is built yet.

## What happens today

The journal is a [room](rooms.md): the Ganymede apps (since ganymede-ui
0.10.0) and JupyterLab with the collaboration extensions edit the same
live notebook the agent's `ganymede mcp` tools edit. So a person's edit
reaches the server at once. What the agent does with it:

- **When you next write to the agent, it sees your edits.** The server
  keeps a read mark per agent: the notebook as the agent last saw it.
  The journal plugin's `UserPromptSubmit` hook asks for the cells
  others added, edited, moved or deleted since that mark, and hands
  them to the agent with your message ([Changes by
  others](mcp.md#changes-by-others)). The skill tells the agent to
  read your cells as input and say how it used them.
- **While the agent is working through a turn, it doesn't.** No tool
  reply mentions your edits. The skill asks the agent to call
  `notebook_read(since)` "after a pause", which it seldom does mid-turn.
- **While the agent is idle, nothing happens.** It waits for your
  next message, however much you change in the notebook.
- **It doesn't know who made a change.** The report says "by a person
  or another agent". Cells the agent wrote carry
  `metadata.ganymede.agent`; your edit to one of them keeps that
  metadata, so it still looks like the agent's cell.
- **Re-runs don't count.** Only cell sources and types are compared.
  If you re-run a cell and its result changes, the agent isn't told.
- **The agent can overwrite your edit.** `cell_edit` replaces a cell's
  whole source with no check. If you changed that cell after the agent
  last read it, your text is lost from the cell (it is kept in
  `metadata.ganymede.agent.history`, which nobody sees).

So the answer is: partly. The agent picks up your edits when you talk
to it, not when you make them.

## What rooms make possible

A room is one live document every client is connected to. The server
sees each edit as it happens, knows which connection made it, and can
tell when the notebook settles. Plain files give none of that: an agent
writing `.ipynb` files only finds changes by re-reading.

That is enough to:

1. say who changed what (the connection that sent the edit);
2. tell the agent during a turn, on its next tool call;
3. tell it while it is idle, if its client can be woken;
4. stop it overwriting an edit it hasn't seen.

## Design

### 1. Who changed what

The server records, per room and per cell, the last connection that
changed the cell's source, and the last that ran it. A connection is
named by what it already says about itself:

- an MCP connection by its client name (`claude-code`), as provenance
  does;
- a room client by its awareness user name (JupyterLab sends one; the
  apps would send the person's name, or "you");
- otherwise "a person".

The change report then reads `"change": ["edited"], "by": "Thomas"`.
This stays in the server's memory, like read marks: nothing new is
written into the notebook. A restart forgets it, and the report falls
back to "a person or another agent".

### 2. Runs count as changes

A cell someone else ran since the agent's mark is reported as `"ran"`,
with its new outputs, beside `"added"`, `"edited"` and `"moved"`. The
agent's own runs move its mark, as its edits do now.

### 3. Tell the agent during a turn

Every `ganymede` tool reply ends with one line when others changed the
notebook since the agent's mark:

```text
@@changed 2 cells by Thomas since your last read: call notebook_read(since: "3f2a9c0d1b7e4a65").
```

It is a pointer, not the changes, so a long edit doesn't bloat every
reply, and the agent decides when to read. This works for any MCP
client, not only Claude Code. The journal plugin's `PostToolUse` hook
can go further and pass the changes themselves, as the prompt hook does.

### 4. Tell the agent while it is idle

Today nothing can start an agent's turn but the person's message. Three
ways to change that:

- **A plugin monitor.** A new `ganymede mcp watch` command prints one
  line per settled change and keeps running. Claude Code plugins can
  declare [monitors](https://code.claude.com/docs/en/plugins/components.md#monitors)
  that run for the whole session; what a monitor prints reaches the
  agent "so Claude can react without being asked". The journal plugin
  would ship one. The docs don't say whether a line wakes an idle
  session or waits for the next turn, so this needs a check by hand.
- **A Claude Code channel.** An MCP server that declares
  `experimental["claude/channel"]` can send `notifications/claude/channel`,
  which arrives in the open session as a message
  ([Channels](https://code.claude.com/docs/en/channels)); the examples
  show it answered while idle. A plugin can declare one. But channels
  are a research preview: Claude Code loads only allowlisted ones, so
  ours needs `--dangerously-load-development-channels` until that
  changes, and they need claude.ai or Console sign-in.
- **MCP resource subscriptions.** The server already lists the open
  notebook as a `notebook://` resource and could send
  `notifications/resources/updated`. It is the standard way and costs
  little, but Claude Code doesn't document doing anything with it, so
  it isn't known to wake anything.

Whichever way, the server should not wake the agent on every keystroke.
It waits until the notebook settles (no edit for a few seconds), and
then only for a change worth a turn (see decision 1).

### 5. Ask the agent from the notebook

The clearest signal is an explicit one. A person writes a Markdown cell
that starts with `@agent` (or the client name, such as `@claude`):

```markdown
@agent this drops the 2019 rows. Check whether that changes the trend.
```

The server reports such a cell as a request (`"change": ["added",
"request"]`), and it is the change that wakes an idle agent. The agent
answers in the notebook, below the request, and marks the cell done
(`metadata.ganymede.request = {"done": "<time>"}`), so it isn't asked
twice. This works in every frontend, JupyterLab and VS Code included,
because it is only a cell. The apps can add a button for it later
("Ask the agent about this cell").

Only a cell a person added or edited through a room client while the
agent's session is open counts as a request, as the attribution in 1
records it. An `@agent` cell already in a file, such as a notebook
someone sent you, one written to disk by another program, or one an
agent or a kernel (through the host API) wrote, is read as text, never
as an instruction, so a notebook can't carry orders to your agent.
After a server restart, with no attribution, no cell counts as a
request until a person edits it again. A request still asks; the agent's tools and grants are the same
as for your message.

### 6. Don't overwrite an edit the agent hasn't seen

`cell_edit` checks the cell against the agent's mark, as the execute
endpoint checks a source hash. If someone changed the cell's source
since the agent last read it, the edit fails with the cell as it is
now and who changed it, and the agent redoes its edit on top. The
failed edit doesn't move the mark. `cell_delete` and `cell_move` check
the same way.

### Presence

The agent could also show in the apps as a collaborator, with its
cursor on the cell it is editing, through the room's awareness. Useful
for watching it work, but not needed for the rest; left for later.

## Decisions

1. **What wakes an idle agent?**
   - (a) Only a request cell (5). Other edits wait for its next turn.
     *Recommended:* you choose when to hand the agent the notebook, and
     it doesn't start work while you are mid-thought.
   - (b) Any edit, once the notebook settles.
   - (c) Nothing: keep today's behavior, add only 1-3 and 6.
2. **How to wake it**, if 1 isn't (c):
   - (a) A plugin monitor running `ganymede mcp watch`, after a check
     by hand that it wakes an idle session. *Recommended:* it ships in
     the plugin and needs no flags; the watch command also serves
     scripts and other agents.
   - (b) A channel, accepting the development flag until channels
     leave preview.
   - (c) Wait: do 1-3 and 6 now, and pick when channels are out of
     preview.
3. **An edit to a cell the agent hasn't seen** (6):
   - (a) Refuse it, the agent redoes it. *Recommended:* nothing of yours
     is lost and the agent sees why.
   - (b) Merge the two texts.
   - (c) Overwrite, as today, keeping your text in the history.
4. **Where attribution lives** (1):
   - (a) In the server's memory. *Recommended:* no new metadata in your
     files, and the agent only needs it while it works.
   - (b) In each cell's metadata, so it survives restarts and other
     tools can show it.

Thomas's choices: 1(a), with 1(b) as a follow-up spike to see how
waking on every settled edit works in practice; 2(a); 3(a); 4(a).

## Order of work

Tracked in [#763](https://github.com/tlockney/ganymede/issues/763):

1. Server: attribution, `"ran"` changes, the `@@changed` line, and the
   edit check (1, 2, 3, 6). Every MCP client benefits.
2. Plugin and skill: pass changes in `PostToolUse`; teach the request
   cell and marking it done (5).
3. Waking an idle agent (4), per decision 2.
4. ganymede-ui: send the person's name in awareness; later, the "Ask
   the agent" button and the agent's presence.
5. Spike: wake on any settled edit (1b), off by default, to compare
   with request cells.
