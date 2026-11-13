## 2026-11-13, who changed a cell, and request cells (#770)

Second step of tracking #763, from
[An agent that responds to your edits](../live-agent-edits.md).

The room now keeps, per cell id, who last changed the cell's source or
type: `Room::authors`, in memory. For a client's update it hashes each
cell's type and source before and after applying it, straight from the
yrs document (no notebook build, no outputs), and credits the cells
that differ to the name in that connection's awareness state
(JupyterLab's `user.display_name`). A reload or merge from the file is
credited to the file. MCP edits go through `Room::edit`, which credits
no one, so `own_edit` calls `Room::set_author` with the client's name
and `person: false`.

Hashing every cell on every client update costs time linear in the
notebook's source text, outputs excluded. Observing yrs deep events with the
connection as origin would avoid it but ties the room to yrs internals.

A request is a Markdown cell whose first word is `@agent`, the client
name, or its first part, with a person as its author since the mark.
The author check is what keeps a planted `@agent` cell (in a shared
notebook, or written by another agent or a kernel) from counting, as
the design review asked. Marking a request done waits for the plugin
step.

The review found that a kernel's host-API edit to a person's cell kept
the person's credit, so planted `@agent` text would read as their
request. `Room::edit`, which every server-side writer goes through, now
credits the cells it changes to "the server" (`person: false`), and the
MCP path renames that to the client with `set_author`. Clients' names
are cleaned (no control characters, 64 characters at most) and quoted
in the `@@changed` line, so a name can't forge that line. A deleted
cell that comes back with the same hash, as on an undo, keeps its
author.
