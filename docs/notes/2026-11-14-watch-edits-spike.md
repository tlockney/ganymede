## 2026-11-14, wake the agent on any edit: a spike (#794)

Thomas asked to try waking the agent on any edit a person makes, not
only on a request cell, as a spike after the request work (#763).

`notebook_requests` takes `edits: true`: besides the open requests, an
`@@notebook {path}` line for each notebook read and an
`@@edit {path, id, by, hash}` line for each other cell a person last
changed. The hash is the read marks' fingerprint (kind and source), so
a poller can tell a change without the reply carrying every cell's
text. An answered request keeps the person as author, so it is listed
too; the watch takes it as it is when it leaves the request list, or
every answer would wake the agent again (review), while the person's
later edits to it still count. The
`@@notebook` lines let the watch take a notebook as it is when first
listed, rather than when it first has a person's cell: in a journal the
agent wrote, that was the person's first edit, which was then never
reported. `ganymede mcp watch
--edits` keeps those hashes per notebook, takes a notebook seen for the
first time as it is, and prints one line per notebook once no cell in
it has changed for `--settle` seconds: who, how many cells, and a
pointer to `notebook_read(since)`. The journal plugin's monitor adds
`--edits` only when `GANYMEDE_WATCH_EDITS` is set. Off by default.

What to find out by trying it: whether a line per settled burst of
edits helps the agent follow along or pulls it off its work, and
whether a settle of a few seconds is too short for prose. Deleted cells
aren't counted, and the line doesn't say what changed: the agent reads
it with `notebook_read(since)`. A follow-up issue decides keep, make
default, or drop.
