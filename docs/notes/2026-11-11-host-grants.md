## 2026-11-11, host API: per-notebook grants and `ganymede grant` (#662)

A notebook's own grant sits in a `host_grants` table in the file id
database, keyed by file id, so it follows a rename made through the
server and is dropped with the file. The store holds it as opaque JSON;
the server's `Grant` type (`read`, `edit_self`) gives it meaning. A key
the notebook sets replaces the `[host]` default for it, which keeps one
rule for both directions: `read=journal` narrows a server whose default
is the whole root, and widens one whose default is `[]`.

The server reads the grant on every call, so `ganymede grant` from
another process takes effect at once. SQLite already answers each file
id lookup, and a host call is rare next to that.

`run` waits for `notebooks.run`, so the command takes only `read` and
`edit_self` for now.
