## 2026-11-20, list open rooms (#893)

Added `GET /api/collaboration/rooms`, which lists the notebook rooms open
now: path, file id, `json`/`notebook`, who is in each, and the last
change. The apps' Journals and Running tabs need to know which notebooks
have an agent in them and when it last wrote; until now a room could be
joined but not listed. [Listing open rooms](../rooms.md#listing-open-rooms)
has the shape.

It is a route on the existing `/api/collaboration` router, so it takes
the same credentials (token, cookie, an allowed tailnet login) and the
same origin checks as the rest, with no new rules. Each room answers a
new `Cmd::Info` on its own command queue, so the list never opens a room
or holds one open, and a room that is busy with a slow save and doesn't
answer in two seconds is left out instead of holding up the list.
Awareness-only rooms have no file and no answer.

Clients are the room's connections. A person's name is what attribution
already uses (`Room::client_name`: the tailnet login, else the awareness
name, else "a person"), so the same `clean_name` applies: control
characters dropped, at most 64 characters. An agent is a `Hold` made for
an MCP connection, which now carries the MCP client name into the room
(`hold_notebook`'s new `Listing` argument: `Person`, `Agent(name)` or
`Unlisted`); the host API's holds for kernels are `Unlisted` and not
listed, since no one is in the room. A
name is listed per connection, so one person with two tabs shows twice.

`last_change` is new state in the room: a time next to the cell authors
`Room` already kept, set when attribution records that a cell's source
or type, or the set of cells, changed. `set_author` then names who made
the change in it, so an agent's edit is the agent's and not "the
server", but only when the server's edit just before it changed cells:
an MCP edit that wrote the same source leaves `last_change` as it was.
It doesn't count outputs, and it is
in-memory like the authors, so it is `null` for a room that no one has
edited since it opened. A room left open after its last client goes (the
one-minute cleanup) stays listed with no clients.

Older servers answer 404, which the docs tell clients to read as
"unsupported", not "no rooms".
