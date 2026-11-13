## 2026-11-13, table and chart output design note (#747)

The direction review found no table viewer for a dataframe and no chart
helper in TypeScript notebooks. Thomas asked for a design note before
any code: [Tables and charts for TypeScript notebooks](../data-output.md).

Reading cdp-kernel's `display.mjs` showed most of the parts exist. It
already writes `application/vnd.dataresource+json` for Polars, which
nothing in Ganymede renders, and a text summary for Vega-Lite charts,
which the apps and `nb publish` don't draw. The example notebooks each
define a `vega.View(...).toSVG()` helper, and the journal skill teaches
linkedom for Plot and warns that a plot with a legend becomes HTML an
agent can't see.

So the note proposes no new MIME type and no new protocol: the table
output reuses the data resource type with capped JSON rows (Arrow IPC
would be base64 in the file, unreadable to diffs and agents), and the
chart helper always yields SVG, redrawing Plot's HTML legend inside it.
Seven decisions are left for Thomas.

Thomas agreed the design and the recommendations the same day and asked
for a check against Python's equivalents. The note's "Compared with
Python" section takes three things from it: head and tail rows in the
fallbacks (pandas, Polars), column types under the names (Polars), and
a size warning for large SVG charts (Altair's `MaxRowsError`, without
refusing to draw).
