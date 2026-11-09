## 2026-11-09, Snapshots of cells for the agent's user (#611)

Thomas wanted a journaling agent to show what the notebook produced in
its chat replies: a chart, a table of results, the code behind them. The
new MCP tool `cell_snapshot(ids)` returns cells as Markdown: the code in a
fence tagged with the notebook's language, each image output saved to a
file and linked by path, HTML tables (pandas, Polars) as Markdown tables
cut to `max_rows`, Markdown output as it is, and other text without ANSI
colours. Images go to `snapshots/` in the private outputs directory, or
with `dir` to a directory under the server root through the contents API
(not in read-only mode). The shaping lives in `mcp/snapshot.rs`, which has
no I/O; the tool writes the files.

The same tool fixes the third rough edge from the Fremont Bridge run: on a
shared `serve --mcp` without `mcp_images` the agent never saw its charts,
and wrote "stayed near 30%" from a table while the chart showed a climb to
44%. `cell_snapshot` returns the images as MCP image content whatever the
server's image setting, since the agent asked for them. The journal skill
and the `journal` prompt now say to look at each chart before writing its
finding, and to paste snapshots when reporting to the user. The
`journal-reviewer` agent gets the tool to check findings against charts.
`images` in `cell_run` replies now uses the same image picker, so an
output with both PNG and JPEG sends only the first.

The HTML table reader is a small scanner over `<tr>`, `<th>` and `<td>`
rather than an HTML parser dependency. It takes the first table in the
output and its first row as the header, which fits pandas and Polars'
HTML. Vega-Lite specs without an SVG or PNG still come out as text.

The reviewer of #612 found that an error's `ename` and `evalue` still
reached the agent with colours (#614): cdp-kernel describes a thrown
non-Error with `inspect`, colours on, so `throw { code: 1 }` arrived as
`{ code: \x1b[33m1\x1b[39m }` in the `@@output` head. Those two fields are
stripped now too, and the per-frame strip of tracebacks went, since the
joined body is stripped anyway.
