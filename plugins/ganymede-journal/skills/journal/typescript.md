# TypeScript in a journal (cdp-kernel)

cdp-kernel runs TypeScript cells on Node (`cdp-node`), Deno, Bun or a
browser. Types are stripped, not checked. Imports from npm use the
`npm:` prefix and are fetched on first use; pin versions so the
notebook re-runs the same way.

## Output

A cell's last expression is its result, shown under the cell. A
trailing semicolon doesn't hide it; a cell ending in a declaration has
no result. `console.log` prints as the cell runs.

The global `jupyter` (a port of `Deno.jupyter`) builds rich output:

```ts
jupyter.md`## Results\n\nAverage: **${avg.toFixed(1)}**`;   // Markdown
jupyter.table(rows, { title: "Weekly failures" });            // a table
jupyter.chart(spec);                                          // a chart, as SVG
await jupyter.sql`select …`;                                  // SQL, shown as a table
jupyter.html`<b>${escapeHTML(text)}</b>`;                     // HTML, not escaped for you
jupyter.svg`<svg xmlns="http://www.w3.org/2000/svg">…</svg>`; // SVG; keep the xmlns
await jupyter.display(value);                                  // show now, mid-cell
await jupyter.display(v, { display_id: "p", update: true });   // update in place
jupyter.image("chart.png");                                    // PNG or JPEG from a file or bytes
```

`jupyter.table`, `jupyter.chart` and `jupyter.sql` need cdp-kernel
0.11.0 or later; `typeof jupyter.chart` is `"undefined"` before it.
cdp-kernel's [jupyter module reference](https://ganymede-server.com/docs/cell-api/jupyter)
has every option.

The agent sees text, Markdown and plain text. HTML is shown with its
tags stripped. Show numbers you want to read back with `jupyter.table`:
its plain text lists the columns, their ranges and the first and last
rows, and `cell_snapshot` turns it into a Markdown table.

## Libraries

Import what a cell needs from npm with an exact version,
`import * as pl from "npm:nodejs-polars@0.18.0"`. The kernel
fetches it the first time a cell runs, on any machine, so the notebook
needs no install step. Import in the cell that first uses it, or in the
module next to the notebook, not in a probe.

## Tables

`jupyter.table` takes an array of row objects, an Arrow table, a Polars
DataFrame or a DuckDB result. People see an HTML table in JupyterLab,
VS Code and the Ganymede apps (apps newer than 0.10.1 let them sort and
page it).

```ts
jupyter.table(weekly.toRecords(), { title: "Weekly failure rate" });
```

A table keeps at most 1,000 rows by default (`limit`, up to 10,000).
Aggregate first; a table of every row is rarely the evidence for a
finding.

## SQL

`jupyter.sql` runs SQL on an in-memory DuckDB database. A `${value}` is
a bound parameter, never part of the query. The result shows as a
table, and has `rows` (plain objects) for code that goes on with it:

```ts
await jupyter.sql.load("runs", "https://example.com/runs.parquet");  // CSV, Parquet, JSON, or rows
const since = new Date("2026-10-01");
const { rows } = await jupyter.sql`
  select date_trunc('week', started) as week, count(*) as runs, count(*) filter (where failed) as failed
  from runs where started >= ${since}
  group by week order by week`;
```

DuckDB reads Parquet over HTTP in ranges, so a query fetches only the
columns it needs. It loads on the first query (`@duckdb/node-api` on
Node, Deno and Bun).

## Polars

```ts
import * as pl from "npm:nodejs-polars@0.18.0";

const df = pl.readCSV(await (await fetch(url)).text(), { quoteChar: '"' });
const weekly = df
  .groupBy("week")
  .agg(pl.col("failed").count().alias("runs"), pl.col("failed").sum().alias("failed"))
  .sort("week");
weekly;                       // shows as a table
weekly.toRecords();           // plain objects, for a chart or jupyter.sql.load
```

`df.head()`, `df.shape`, `df.columns` and `df.schema` are quick probes
for `cell_run(record: false)`.

## Charts

`jupyter.chart` draws the chart in the kernel as one SVG, with its title
and legend inside, so you can see it (the server sends SVG to you as a
PNG) and it shows the same in the apps, JupyterLab and a published page.
Pass a [Vega-Lite](https://vega.github.io/vega-lite/) spec:

```ts
jupyter.chart({
  title: "Weekly failure rate",
  data: { values: weekly.toRecords() },
  mark: "bar",
  encoding: {
    x: { field: "week", type: "ordinal" },
    y: { field: "rate", type: "quantitative", axis: { format: "%" } },
  },
  transform: [{ calculate: "datum.failed / datum.runs", as: "rate" }],
});
```

Or [Observable Plot](https://observablehq.com/plot/): pass the module
and the options you would give `Plot.plot`. No DOM is needed.

```ts
import * as Plot from "npm:@observablehq/plot@0.6.17";

jupyter.chart(Plot, {
  title: "Failures by suite",
  color: { legend: true },
  y: { grid: true },
  marks: [Plot.barY(bySuite, { x: "week", y: "failed", fill: "suite" }), Plot.ruleY([0])],
});
```

A color legend is drawn into the SVG; legends of other scales are left
out, so name those in the finding. A chart is stored with every point:
aggregate or sample first (`jupyter.sql` is good at this).

## Widgets

`jupyter.widget` makes an [anywidget](https://anywidget.dev) model the
person can drive from the notebook. The view is a small ES module that
runs in their frontend; the kernel keeps the state and reacts to it. To
let them move a threshold on a chart, keep the plotting in the kernel
and send the view the SVG:

```ts
const svgOf = async (chart) => (await chart[jupyter.$display]())["image/svg+xml"];
const draw = (min: number) =>
  svgOf(jupyter.chart({
    data: { values: weekly.toRecords().filter((d) => d.runs >= min) },
    mark: "bar",
    encoding: { x: { field: "week", type: "ordinal" }, y: { field: "failed", type: "quantitative" } },
  }));

const picker = jupyter.widget({
  esm: `export default {
    render({ model, el }) {
      const slider = Object.assign(document.createElement("input"), { type: "range", min: "0", max: "200" });
      const chart = document.createElement("div");
      const update = () => { slider.value = String(model.get("min")); chart.innerHTML = model.get("svg"); };
      slider.oninput = () => { model.set("min", slider.valueAsNumber); model.save_changes(); };
      model.on("change:svg", update);
      update();
      el.append(slider, chart);
    },
  };`,
  state: { min: 0, svg: await draw(0) },
});
picker.on("change:min", async (min) => {
  const svg = await draw(Number(min));
  if (picker.get("min") === min) picker.set("svg", svg);  // a newer value may have come in meanwhile
});
picker;
```

The chart draws asynchronously, so check that the value is still the
latest before showing it: otherwise a fast slider can let an older chart
land last.

Inside `esm`, a `${...}` is filled in by the kernel: write `\${...}` for
one the view should see. The agent sees only `text/plain` for a widget,
and a published page shows only that text, so follow the widget with a static
chart for the value the finding names. cdp-kernel's
[widgets guide](https://github.com/tlockney/cdp-kernel/blob/main/docs/widgets.md)
has more: progress bars, binary data, messages.
