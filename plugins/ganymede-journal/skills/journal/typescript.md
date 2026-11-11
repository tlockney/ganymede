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
jupyter.html`<b>${escapeHTML(text)}</b>`;                     // HTML, not escaped for you
jupyter.svg`<svg xmlns="http://www.w3.org/2000/svg">…</svg>`; // SVG; keep the xmlns
await jupyter.display(value);                                  // show now, mid-cell
await jupyter.display(v, { display_id: "p", update: true });   // update in place
jupyter.image("chart.png");                                    // PNG or JPEG from a file or bytes
```

The agent sees text, Markdown and plain text. HTML is shown with its
tags stripped, so prefer Markdown tables over HTML for numbers you want
to read back. A short summary reads well as one Markdown display:

```ts
jupyter.md`### Weekly failure rate

| week | runs | failed |
|---|--:|--:|
${weekly.toRecords().map((r) => `| ${r.week} | ${r.runs} | ${r.failed} |`).join("\n")}`;
```

## Libraries

Import what a cell needs from npm with an exact version,
`import * as Plot from "npm:@observablehq/plot@0.6.17"`. The kernel
fetches it the first time a cell runs, on any machine, so the notebook
needs no install step. Import in the cell that first uses it, or in the
module next to the notebook, not in a probe.

## Polars

```ts
import * as pl from "npm:nodejs-polars@0.18.0";

const df = pl.readCSV(await (await fetch(url)).text(), { quoteChar: '"' });
const weekly = df
  .groupBy("week")
  .agg(pl.col("failed").count().alias("runs"), pl.col("failed").sum().alias("failed"))
  .sort("week");
weekly;                       // shows as a table
weekly.toRecords();           // plain objects, for Plot or Markdown
```

`df.head()`, `df.shape`, `df.columns` and `df.schema` are quick probes
for `cell_run(record: false)`.

## Observable Plot

Plot needs a DOM. On Node, give it linkedom's:

```ts
import * as Plot from "npm:@observablehq/plot@0.6.17";
import { parseHTML } from "npm:linkedom@0.18.9";
const { document } = parseHTML("<html></html>");

const chart = Plot.plot({
  document,
  y: { grid: true, percent: true },
  marks: [Plot.barY(weekly.toRecords(), { x: "week", y: (d) => d.failed / d.runs })],
});
chart;
```

An element whose `outerHTML` starts with `<svg` shows as SVG, which the
server can send to you as a PNG. A plot with a legend
(`color: { legend: true }`) is a `<figure>` and shows as HTML, which you
can't see: end the cell with `chart.querySelector("svg")` instead, and
name the colors in the finding.

## Vega-Lite

```ts
await jupyter.display({
  "application/vnd.vegalite.v5+json": spec,
  "text/plain": "chart",
}, { raw: true });
```

JupyterLab renders it; the agent sees only the text. For a chart you
need to read, use Plot as SVG.

## Widgets

`jupyter.widget` makes an [anywidget](https://anywidget.dev) model the
person can drive from the notebook. The view is a small ES module that
runs in their frontend; the kernel keeps the state and reacts to it. To
let them move a threshold on a chart, keep the plotting in the kernel
and send the view the SVG:

```ts
const draw = (min: number) =>
  Plot.plot({
    document,
    marks: [Plot.barY(weekly.toRecords().filter((d) => d.runs >= min), { x: "week", y: "failed" })],
  }).outerHTML;

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
  state: { min: 0, svg: draw(0) },
});
picker.on("change:min", (min) => picker.set("svg", draw(Number(min))));
picker;
```

Inside `esm`, a `${...}` is filled in by the kernel: write `\${...}` for
one the view should see. The agent sees only `text/plain` for a widget,
and a published page shows only that text, so follow the widget with a static
chart for the value the finding names. cdp-kernel's
[widgets guide](https://github.com/tlockney/cdp-kernel/blob/main/docs/widgets.md)
has more: progress bars, binary data, messages.
