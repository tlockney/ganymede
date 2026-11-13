## 2026-11-13, journal skill uses jupyter.chart (#788)

The journal skill's TypeScript guide now draws charts with
`jupyter.chart` (a Vega-Lite spec, or Observable Plot) and shows tables
with `jupyter.table`, and has a section on `jupyter.sql`, all from
cdp-kernel 0.11.0. The linkedom setup, the `chart.querySelector("svg")`
workaround for Plot legends and the hand-built Markdown table are gone:
`jupyter.chart` draws the title and color legend into one SVG the agent
can see, and a table's plain text, or its Markdown from `cell_snapshot`
(#775), is what the agent reads back. The widget example renders its
SVG through the chart's display bundle (`chart[jupyter.$display]()`).

SKILL.md steps 14 to 16 point at these and name the cdp-kernel version
they need. Each snippet was run on cdp-kernel 0.11.0 (`cdp-node`) with
`ganymede run`. The two example journals under `examples/journals/`
still use linkedom; they are saved runs, left as they are.
