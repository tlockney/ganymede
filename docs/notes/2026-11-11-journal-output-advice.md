## 2026-11-11, journal plugin: display output, widgets and plotting libraries (#710)

The journal skill said how to draw a chart with Observable Plot and
little else about output. It now has an "Output, charts and widgets"
section: format results with the kernel's display module (`jupyter` in
cdp-kernel, `IPython.display` in Python), pull in a pinned plotting
library when a result needs a chart, and add a widget when a person
watching would explore a result. A new `python.md` sits beside
`typescript.md` for ipykernel.

The skill now gives the reason for its TypeScript default: `npm:`
imports resolve when the cell runs, so a journal that needs libraries
re-runs anywhere without an install step, where a Python kernel has only
what its environment has (or what a recorded `%pip install` cell adds).

Widgets are for the person. The agent gets only a widget's `text/plain`,
and `notebook_publish` leaves widgets out (`html.rs` falls back to a
note), so the skill and the reviewer ask for a static chart or table
beside any widget a finding depends on. Charts drawn in the frontend
(Vega-Lite, Altair, Plotly) have the same limit for the agent.

Checked outside a kernel: the TypeScript widget example's kernel side
against cdp-kernel's `runtime/widget.mjs` with a simulated frontend
update (the listener redraws the SVG and sends only `svg`), its view
module parses, and the Markdown table example renders. The Python
examples ran with the pinned versions (polars 1.31.0, matplotlib
3.10.3, ipywidgets 8.1.7). Not run end to end in an agent session.
