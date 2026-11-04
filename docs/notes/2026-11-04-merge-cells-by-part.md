## 2026-11-04, merge cells part by part (#279)

`ganymede_documents::merge` treated a cell as one value. A person typing
in a cell while an agent ran it through the contents API (`nb execute`)
lost the edit: the room changed `source`, the disk changed `outputs` and
`execution_count`, both changed "the cell", and the disk won it whole.

A cell present on all three sides now merges in parts: `source`; the
outputs with the execution count, as one part, since a run changes both
and mixing one side's outputs with the other's count would describe no
run; each `metadata` key; and each other key. A change of `cell_type`,
or a cell added or deleted, still merges whole. Notebook-level `metadata`
merges key by key too, so a kernelspec change on disk and a title edit
in the room both survive. A conflict in any part still reports the cell
(`cell <id>`) or `metadata`, so a forced save that lost a part answers
`failed` as before. Merged cells keep the disk's key order.

This is the last code item from #279; its nits (session path
normalization, hash check before parsing on watcher events, SQLite off
the async workers, a shared path-prefix rule) are left open there.
