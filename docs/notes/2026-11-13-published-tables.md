## 2026-11-13, tables in published pages (#756)

`ganymede nb publish` now draws a table output
(`application/vnd.dataresource+json`, from cdp-kernel's `jupyter.table`
and `jupyter.sql`, and pandas with `display.html.table_schema` on) from
its JSON rows, before trying the bundle's other entries. Before, the
page showed the HTML fallback in a frame, which has a fixed height and
can't share the page's type and colors.

The table shows the first 100 rows in a scrolling box with a sticky
header, the column's Table Schema type under its name, and numbers
right-aligned. Below it a line gives the count: "9 rows", or "100 of
1,461 rows" from `ganymede.rows`, the source's row count, when there
are more. pandas writes no row count and embeds at most its
`max_rows`, so its tables say "60 rows embedded" rather than claim to be
whole. pandas also keeps column labels as they are, so a field may be
named `0`; names that aren't strings are written as JSON text, which is
how the rows key them. Values go in as escaped text after the page's
redactions, so a table is as safe as a text output. Rows may be objects
or arrays; a missing cell is empty, a JSON null shows as `null`. An
entry without `schema.fields` falls back to the bundle's other entries.

Checked by hand on a notebook run on cdp-node with tables, SQL results
and charts, in light and dark.
