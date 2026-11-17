## 2026-11-17, execute runs the room's source (#843)

The Shortcuts and link runs in ganymede-ui hash the saved file's source,
so cells the person hasn't saved don't run, and they refuse nbformat 4.4
notebooks for want of cell ids (ganymede-ui#301, #403).

The room already had what was needed: `schema::insert_cell` gives id-less
cells a random id on load, and `notebook_with_ids` keeps them, so the
`wait` reply (#566) already carried room ids for 4.4 cells. The gap was
the required `source_hash` and no way to find the ids.

`/api/kernels/{id}/execute` now takes entries without a hash (the room's
source runs), `{"tag"}` entries and `"cells": "all"`. Selection happens in
the room task, in the same `room.edit` as the hash checks and the output
clear, so the cells can't change between choosing and running, a refused
batch clears nothing, and the ids are the room's own. `cell_not_found`
names the id or tag; for 4.4 it says the ids last only while the room is
open. Those ids die with the room (60 s after the last client, with
nothing running), so a client must not keep them between runs.

`schema::keeps_no_ids` holds the nbformat 4.4 rule that `read` used
inline. The MCP tools already ran hashless; only their error text can
change. `ganymede execute`, `/api/notebooks/run` and the host API are
unchanged.

Against v0.11.0 the endpoint answers `400 source_hash is required`, so
an app using this falls back to hashing the saved file.
