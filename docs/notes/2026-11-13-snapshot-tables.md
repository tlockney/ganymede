## 2026-11-13, snapshots read table outputs from their rows (#774)

`cell_snapshot` turned a table into Markdown from its HTML, which suited
pandas and Polars. cdp-kernel 0.11.0's `jupyter.table` HTML puts each
column's type in `<small>` under its name and ends a long table with a
one-cell gap row, so an agent got "moonstring" headers, the gap row as
data, and a row count from the HTML rows. A snapshot now reads
`application/vnd.dataresource+json` when an output has one: names and
types from the schema, rows from the data, the total from cdp-kernel's
`ganymede.rows`. HTML stays the fallback.
