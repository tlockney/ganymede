# Live editing and document rooms

A document room is a notebook held live in the server, so that several
clients edit it at once and see each other's edits as they type: two
JupyterLab tabs, two people, or a person and an agent. The file on disk
stays the source of truth. The room loads it, saves to it, and takes in
changes other programs make to it.

Ganymede speaks the same protocol as `jupyter-collaboration`, so
JupyterLab's collaboration extensions work unchanged, and nb-cli and
[`ganymede mcp`](mcp.md) edit and run notebooks through rooms too.

![A document room. Clients such as JupyterLab tabs, a second person, nb-cli and ganymede mcp connect to ganymede serve over websockets and exchange yjs updates with one document room per open notebook. The room holds the notebook as a yrs document in jupyter_ydoc's schema, named for the file's id. It autosaves to the notebook file one second after the last edit, with If-Match on the hash it last read. The file stays the source of truth: other programs, such as an agent, git pull, a sync service or an editor, write it, and the room's watcher takes their changes in once they settle for half a second, at most five seconds after the first, merging them cell by cell.](images/rooms/overview.svg)

## Turn it on in JupyterLab

Install the collaboration extensions into the JupyterLab that Ganymede
serves:

```sh
ganymede lab extension install jupyter-docprovider jupyter-collaboration-ui
```

From then on JupyterLab opens notebooks through rooms: open the same
notebook in two tabs, or share the server with a second person, and each
sees the other's edits and cursor. Without the extensions JupyterLab reads
and writes the file per request, as with `jupyter_server` on its own.

## How the room and the file stay in step

The room keeps the notebook as a `yrs` document (the Rust port of Yjs), in
the schema `jupyter_ydoc` and JupyterLab's `@jupyter/ydoc` share. It
autosaves a second after the last edit, with an `If-Match` on the hash it
last read, so it never overwrites a change it hasn't seen.

The room watches its file's directory (inotify, FSEvents, or Windows'
change notifications; elsewhere it polls every second) and takes in
changes other programs make: an agent, `git pull`, a sync service, an
editor. It looks once events have settled for half a second, and at most
five seconds after the first unsettled event, so a file that a program
rewrites constantly still reaches the room.

A change that arrives while the room has unsaved edits is merged, cell by
cell:

- The room keeps its edits and takes the other program's changes to other
  cells, and the cells it added or deleted.
- Within a cell both changed, the source, the outputs with the execution
  count, and each metadata key merge separately. A source edit in the room
  survives an agent running that cell on disk.
- Where both changed the same part, or one deleted a cell the other
  changed, the file on disk wins that part, and the server logs the cell.
- Notebook metadata merges key by key the same way.
- Notebooks older than nbformat 4.5 have no cell ids, so for them the file
  wins outright, as in `jupyter-collaboration`.

![Merging a file change into a room with unsaved edits, cell by cell. A cell only the room edited keeps the room's source. A cell only the file changed, for example with new outputs, takes the file's version. A cell the room edited and another program ran keeps the room's source and takes the file's outputs and execution count. A cell both edited in the same part, or one deleted while the other changed it, takes the file's version and the server logs the cell. A cell the file added is added. Source, outputs with execution count, and each metadata key merge separately; notebook metadata merges key by key. Notebooks before nbformat 4.5 have no cell ids, so the file wins outright. The room saves the merge at once.](images/rooms/merge.svg)

After taking in a change from the file, the room saves its own edits at
once. A client's explicit save whose merge lets the file win a cell the
room had changed (by any client, or by execution outputs) saves the merge
and answers `failed`, since that change isn't in the file.

A delete through the contents API closes the room's clients with code
4404, the code a reconnect gets for the dropped file, and the room
doesn't write the file back.

### Trust

The room signs the notebook when it saves it, as jupyter_server signs a
notebook a frontend saves with every cell trusted, so it opens trusted
next time. A code cell's outputs are trusted when they came from a run
on this server: through the [execute endpoint](#running-cells-into-a-room)
or `ganymede mcp`, or written by a client running the cell. Outputs read
from a file nobody signed, when the room opened it or merged it in from
disk, are not: the notebook stays unsigned until each of those cells runs
again through the server, or someone trusts the notebook (`ganymede
trust`, or Trust Notebook in a frontend), which holds with the room
open. A client that writes back the very outputs the unsigned file had
doesn't make them trusted.

## Connections

The server pings each room's websocket every 30 seconds and drops a
client it hasn't heard from, not even a pong, for 90 seconds, so a
connection left half-open by a sleeping laptop or a lost network doesn't
keep its room open. A client that falls 10,000 messages behind, or
`max_body_size` bytes of messages behind, is closed with code 1008:
each client holds its own copy of what it hasn't read, and the kernel
channels drop output past that point, but a
room can't skip an update; a close the room sends (4404 for a deleted
file) still reaches it. A message larger than `max_body_size` closes the
socket. A write to a client may take 30 seconds plus one more for every
64 KiB, and a write that long counts as hearing from the client, so a
large notebook's first sync gets through a slow link. A client
reconnects and resyncs as after any drop.

## Listing open rooms

`GET /api/collaboration/rooms` lists the notebook rooms open now, so a
client can show which notebooks someone is in and when one last changed:

```json
{"rooms": [{
  "path": "analysis/trends.ipynb",
  "file_id": "6f0c...",
  "format": "json",
  "type": "notebook",
  "clients": [
    {"name": "Thomas", "kind": "person"},
    {"name": "claude-code", "kind": "agent"}
  ],
  "last_change": {"at": "2026-11-20T09:41:07.512Z", "by": "claude-code", "person": false}
}]}
```

- `path` is the notebook's API path, the one the contents API takes, and
  follows a rename. `file_id` is the id in the room's name
  (`json:notebook:<file id>`).
- `clients` has one entry for each connection. A `person` is a room
  client, named as its edits are credited (see
  [Who changed what](live-agent-edits.md#1-who-changed-what)): the
  tailnet login the server knows it by, else the name in its awareness
  state, else `a person`. An `agent` is an [MCP](mcp.md) connection
  holding the room open, named by the client name it gave when it
  connected. A name is client-supplied text, cleaned before it is sent:
  control characters dropped, whitespace collapsed, at most 64
  characters. People come before agents.
- `last_change` is when, as UTC with milliseconds, and by whom, a cell's
  source or type, or the set of cells, last changed while the room was
  open; `by` is the same kind of name, and `person` is true when the
  change came from a room client rather than the server, an agent, or the
  file (`another program (the file)`). It is `null` before the first
  change, and a run's outputs don't count. It isn't kept across the
  room closing.
- A room with no clients stays listed, with `clients` empty, until it
  closes (a minute after its last client leaves, as above). Rooms that
  carry awareness only, such as JupyterLab's global awareness room,
  aren't listed. Rooms come ordered by `path`.

The route takes the same credentials as the other `/api/collaboration`
routes (the token, its login cookie, or an allowed tailnet login) and
only `GET`. It lists paths and the names of people connected, so it goes
only to callers the API lets in.

Servers from before this route answer `404` (and a server whose
authentication refuses the caller, `403`). A client treats `404` as "this
server can't list rooms", not as "no rooms open".

## File ids

A room is named for its file's id, not its path. Ids are kept in
`ganymede_file_ids.db` in the Jupyter data dir, so a client keeps its room
across a server restart. A rename or delete through the contents API
moves or drops the id, and a room open on a renamed file follows it. A
file moved by another program gets a new id. When another program
deletes or moves a file with an open room, the room closes its clients
with 4404, as for a delete through the API. If the database can't be
read or written, ids handed out meanwhile are kept in memory and resolve
until the server stops.

`POST /api/fileid/index?path=` gives a file's id, as
`jupyter-server-documents` does.

## Running cells into a room

The server can run cells and write their outputs into the room itself,
with `jupyter-server-documents`' execute endpoint. The outputs reach every
client as they arrive, and reach the file with no client connected. This
is how nb-cli's remote mode and `ganymede mcp` run cells.

```http
POST /api/kernels/{kernel_id}/execute
```

```json
{"document_id": "json:notebook:<file id>",
 "cells": [{"cell_id": "...", "source_hash": "<MurmurHash2 of the source, seed 0>"}],
 "request_id": "...", "previous_request_id": "..."}
```

The server checks each cell's source against its hash, clears the cells,
queues them as one batch, and answers `200 null`. It answers `409
{"error": "source_mismatch", "cell_id"}` if a cell changed since the
client hashed it, and `408` if `previous_request_id` wasn't queued within
ten seconds. A cell that fails stops the rest of its batch, and so does
one that [changed while queued](#a-cell-that-changed-while-queued).

![Running cells into a room. A client such as nb-cli or ganymede mcp sends POST /api/kernels/{id}/execute with the document id and each cell's id and source hash (MurmurHash2, seed 0). Ganymede checks each cell's source in the room against its hash: if one changed it answers 409 source_mismatch, and if the previous request wasn't queued within ten seconds it answers 408, and nothing runs. Otherwise it clears the cells, queues them as one batch and answers 200 null. It sends the cells to the kernel one after another; a cell that fails stops the rest of the batch. As outputs arrive on iopub, Ganymede writes them and the execution count into the room, so every client sees them and autosave writes them to the file even with no client connected.](images/rooms/execute.svg)

### A cell that changed while queued

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

A `source_hash` is checked twice: when the request arrives (the `409`
above), and again when the cell's turn comes, since a batch can wait
behind other runs. If the room's source then no longer has the hash, the
cell is not run. It is skipped like a cell after a failure: the rest of
its batch is skipped too, and the cell goes `idle` with no outputs and no
new execution count.

The cell shows why in the room: `execution_skipped` is
`"Cell changed while queued"`. Like `execution_by` it is in the room
only and is never saved or returned by `GET /api/contents`. It is removed
when the cell is queued again, when its source or type is edited (by a
client, the server or a change to the file), and when a client runs the
cell itself. The cells skipped after it don't get it,
nor do cells skipped after a failure; a cell queued twice shows it only
when its last queued run was skipped for this.

With `wait`, the answer lists these cells, in the order they were to run,
and leaves the key out when there are none:

```json
{"finished": true, "cells": [...],
 "skipped": [{"cell_id": "...", "error": "source_changed_while_queued",
              "message": "Cell changed while queued"}]}
```

A cell with no `source_hash`, or chosen by tag or `"all"`, is not
guarded: it runs the source it has at its turn, unsaved edits included.
So do the MCP tools that run cells.

A cell the server runs through the room records the hash of the source
that ran in its metadata, saved with the notebook:

```json
{"metadata": {"ganymede": {"ran_source_hash": "1234567890"}}}
```

It is the same format as `source_hash`, and a cell edited while it ran
keeps the hash of the source that ran. Outputs belong to an older source
when `ran_source_hash` differs from the hash of the cell's current
source. The cell's other metadata, and the other keys under its
`ganymede`, are left alone, and the notebook's `clean_run` stamp stays:
cell metadata doesn't change a run's fingerprint.

Ganymede's executor records the key too: `ganymede execute`, a
notebooks run (`POST /api/notebooks/run`) and the host API write the
hash of the source they ran, and the copy a notebooks run saves drops
the key from cells the run never started. Tools outside Ganymede —
nbconvert, papermill, VS Code, a plain `PUT` of the file — keep
whatever key the notebook has, so the key says which source ran only
when Ganymede did the last run.

A client that runs a cell itself, such as
JupyterLab, writes the outputs and the execution count into the room, and
the server doesn't see which source they came from. So when a client's
update changes a code cell's `execution_count` or outputs (clearing them
too), the room removes the cell's `ran_source_hash`: a cell without the
key says nothing about which source its outputs came from. A change to
the file keeps what the file has. The hash is in the cell's metadata, not
each output's, because stream and error outputs have no metadata, and a
cell can run without any output.

### Who runs a cell

A cell a run has queued shows `execution_state: running` in the room, and
`idle` once its last queued run ends (a cell that was skipped after an
earlier cell failed, or was deleted, ends its run too). Ganymede adds
`execution_by`, the name of who runs the cell now through the execute
endpoint or `ganymede mcp`. It is in the room only: it is never saved to
the notebook file or returned by `GET /api/contents`, and Ganymede
removes it when the last of those runs ends.

A client that runs a cell itself, such as JupyterLab sending an execute
request on the kernel's websocket, writes `execution_state` on its own
and sets no name; the server doesn't see that run. When the two overlap,
the cell can name a queued agent's run while the person's run is still
on the kernel, or show `idle` with a name until the queued run ends. So
read `execution_by` only while the cell is `running`, as the name of the
earliest of Ganymede's queued runs of that cell not yet ended.

The name comes from the entry point:

- The execute endpoint takes, in order: the caller's tailnet login (see
  [Security](security.md#tailnet-identity)), then an optional string `by` in the request
  body, then "an API client". A `by` that isn't a string is a `400`.
  `client_id` is still accepted and ignored.
- An MCP tool that runs cells in the room (`cell_run` when it records a
  cell, `cell_edit` with `run`, `notebook_run_all`) uses the name the MCP client gave when it connected.

Only a tailnet login is vouched for: `by` and an MCP client's name are
whatever the caller sends, so any client with the token can give any
name. Every name is cleaned like a [client's name in the list of rooms](#listing-open-rooms):
control characters dropped, whitespace collapsed, at most 64 characters.
One with nothing left yields to the next source.

A cell queued behind another run of itself keeps showing the run in
progress. When that run ends, the cell shows whoever queued next, and
goes idle after the last. `by` is one of Ganymede's extensions to the
execute body, with `wait` and the cell selectors.

### Run from a clean kernel

A notebook that was last run top to bottom on a fresh kernel carries
`metadata.ganymede.clean_run` in its notebook metadata, so a reader (the
journal view's "reproduced from a clean kernel" stamp) can tell:

```json
{"metadata": {"ganymede": {"clean_run": {
  "at": "2026-11-20T18:04:12.331Z", "by": "Claude Code", "kernel": "python3"}}}}
```

`at` is when the run ended, in UTC. `by` is the name the MCP client gave
when it connected (cleaned like the [names in the room](#who-runs-a-cell)).
`kernel` is the name of the kernelspec the kernel ran. Other keys under
`metadata.ganymede` are left alone.

Only the MCP tool `notebook_run_all` sets it, and only when the run

- restarted the kernel (`restart` is true, the default),
- finished before its `timeout`,
- ended every cell without a failure: an error, a reply of `aborted`, or
  a kernel that went all stop the run short,
- left the notebook as it began (no cell edited, added, deleted or moved
  while it ran), and
- gave the code cells that have code an `execution_count` each, from 1 and
  in order, each one more than the one before, so work on the kernel
  before the first cell, a restart or another run of a cell during the run
  leaves no stamp.

Any other `notebook_run_all` removes the stamp the notebook had, even one
that runs no cells (`restart: false` on a notebook with no code); one that
fails before it gets to run, such as a kernel that won't restart, leaves
it as it was. A client's own Run All, in JupyterLab
or the apps, goes through the kernel's websocket, which the server
doesn't see as a run of the notebook, so it never sets the stamp. It
removes it, though: those clients blank a cell's count when it starts
(JupyterLab's `clearExecution`), so any run from them changes it.

The room removes the stamp when what a run depends on or leaves behind
changes, whoever changes it: a cell's source or type edited (Markdown
cells too), a cell added, deleted or moved, or a code cell's
`execution_count` changed. A run from JupyterLab or the apps changes it,
since they blank the count when a cell starts. A run the server executes
(the execute endpoint, the MCP tools) writes the count only when the cell
finishes, so re-running the cells from the top after a restart gives the
same counts and leaves the stamp. Outputs, `execution_state`, cell metadata and other notebook
metadata change without clearing it. The removal is an edit by the
server like any other: every client sees it and the notebook is unsaved
until it is saved.

The same holds for a change that comes in through the file while a room
is open, such as a `PUT /api/contents` from an editor or `nb-cli`: if it
changes those things and the file still has the stamp the room had, the
room drops the stamp and saves the file without it. A file that carries
a different stamp, or a change that leaves the run standing, leaves the
stamp as the file has it.

The stamp is saved with the notebook, unlike `execution_by`. A file that
holds one opens with it, and it stays until a change above removes it.
Nothing checks it against the cells of a file when a room opens: one
edited while no room was open for it keeps a stamp that no longer holds.

### Waiting for the cells

Ganymede adds an optional `wait` field, in seconds (at most 3600), for
clients that can't follow the room, such as the apps' Shortcuts actions.
With it, the server answers once the batch has run or the time is up,
with each requested cell as the room has it, before the room saves:

```json
{"finished": true, "cells": [{"cell_type": "code", "id": "...", "execution_count": 1, "outputs": [...], "...": "..."}]}
```

`finished` is false when the time ran out; the cells keep running and
the reply shows them as they were. Without `wait`, the answer is `200
null`, as in `jupyter-server-documents`.

### Running the room's current source

The saved file can lag the room, so a client that hashes the file's
source runs stale code or gets a `409`. Ganymede lets the room pick the
cells instead. An entry without `source_hash` (or with `null`) runs the
room's current source, unsaved edits included. A hash, when given, is
still checked.

```json
{"document_id": "json:notebook:<file id>",
 "cells": [{"cell_id": "a"}, {"cell_id": "b", "source_hash": "123"}, {"tag": "report"}],
 "wait": 60}
```

- `{"tag": "x"}` runs every code cell whose `metadata.tags` has `x`.
- `"cells": "all"` runs every code cell with source, in order. With none,
  the answer is `{"finished": true, "cells": []}` (`null` without `wait`).
- A tag or `"all"` skips a cell the batch already runs. A cell id
  given twice runs twice, as in `jupyter-server-documents`.
- The room resolves the cells and checks them in one step. If any is
  refused, none runs and no outputs are cleared.

With `wait`, the reply lists the cells that ran, with the room's ids. A
notebook saved as nbformat 4.4 or older has no cell ids on disk, so a
client learns its ids from that reply (or `"all"`). They are the room's,
are never saved, and last only while the room is open (a minute after the
last client leaves, if nothing is running).

An unknown id is `400`:

```json
{"message": "Cell \"zz\" not found in document", "reason": null, "error": "cell_not_found", "cell_id": "zz"}
```

For a 4.4 notebook the message adds that its ids are the room's. A tag
that matches no code cell is `cell_not_found` with `tag` in place of
`cell_id`. With no room open, the room is opened from the file, as
before. A read-only room (see below) refuses as before. The latest release
answers `400` `source_hash is required`.

## Two servers on one notebook

On one machine, the first server to open a room on a notebook owns it
while the room is open: it holds a lock in the Jupyter runtime dir's
`ganymede-rooms/`, named for the file's path (`flock`, or `LockFileEx`
on Windows). Another server's room on the same file is read-only. Its
contents API reports the notebook as not `writable`, which JupyterLab
shows as read-only. The room takes in the owner's saves, never saves,
and undoes any edit a client sends, for every client. It also refuses
to run cells (`POST /api/kernels/<id>/execute` answers 400, and the MCP
run tools return an error), since their outputs would never be saved.
Once the owner's
room closes or its server exits, the other room takes the lock within a
second and saves its own edits from then on.

![Two servers on one notebook on the same machine. Server A opened a room on the file first, so it holds a lock in the runtime dir's ganymede-rooms folder, named for the file's path, while the room is open; its JupyterLab clients edit, and it saves and runs cells. Server B's room on the same file is read-only: it takes in A's saves through its watcher, never saves, undoes edits its clients send, refuses to run cells with 400, and its contents API reports the notebook as not writable. B checks the lock each second and takes it once A's room closes or A exits. Across machines, as with a synced folder, there is no lock and rooms merge through the file. An app that embeds the server takes no lock.](images/rooms/two-servers.svg)

A room that was writable and finds another server owns its file now
(after a rename, or once the file is reachable by a path another server
locked) saves its edits before it turns read-only.

Across machines, as with a notebook in a synced folder, there is no lock,
and rooms merge through the file as described above. An app that embeds
the server takes no lock.

## Limits

- Notebooks only: a text file opened collaboratively fails to load.
- No forking, timeline or chat.
- Frontends that don't open notebooks through a room see a change made on
  disk only the next time they open the notebook
  ([Known gaps](known-gaps.md#notebooks)).

[The iPad use case](ipados.md) explains why the file, not the room, is the
source of truth.
