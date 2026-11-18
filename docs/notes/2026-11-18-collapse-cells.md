## 2026-11-18, the agent saves helper cells collapsed (#865)

Journal helper cells (#863) hold types and utility functions a reader
can skip, but the agent had no way to collapse a cell: none of the MCP
tools set cell metadata beyond provenance and the dropped-outputs mark.

`cell_run` and `cell_add` now take `collapse: true`, which saves the new
cell with `metadata.jupyter.source_hidden: true`, the key JupyterLab's
collapser writes and the apps' disclosure button reads (ganymede-ui
#564). Only new cells: collapsing an existing one would need a metadata
edit with the same conflict check as `cell_edit`, and the journal makes
helpers as new cells. An older server ignores the unknown argument, so
the skill can name it without a version check; the cell is just
expanded there.

The published page ignores the key: it folds all code by default, and
`code: shown` is a reader's choice to see all of it.
