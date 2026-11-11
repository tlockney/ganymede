# Examples

Notebooks to read before running anything: each page shows the code and
the outputs it saved.

## Agent journals

Two analyses Claude Code kept as notebooks with the
[ganymede-journal plugin](agent-journals.md), in TypeScript on
cdp-kernel. Each page starts with what the person asked; the notebook
keeps every step, the dead ends and the revised findings.

| Journal | The question |
| :--- | :--- |
| [Fremont Bridge](examples/journal-fremont.md) | Have bike commutes over Seattle's Fremont Bridge come back since 2019? |
| [Oklahoma earthquakes](examples/journal-oklahoma.md) | Did Oklahoma's earthquakes stop once wastewater injection was cut back? |

They were kept on 2026-11-09 and re-run on Ganymede 0.8.0 and
cdp-kernel 0.10.0 with freshly downloaded data. The notebooks, the
scripts that fetch their data, and `run.sh`, which re-runs them, are in
[examples/journals/](https://github.com/tlockney/ganymede/tree/main/examples/journals).

## The apps' example notebooks

The Mac, iPad and iPhone apps copy these TypeScript notebooks into a
folder of your choice with **Open Example Notebooks**. They run on the
TypeScript kernel the apps run themselves, with no server.

| Notebook | What it shows |
| :--- | :--- |
| [Start Here](examples/01-start-here.md) | Cells, state, and the kinds of output a cell can show |
| [Web APIs](examples/02-web-apis.md) | `fetch` from public APIs, tables, progress that updates in place |
| [Charts and Diagrams](examples/03-charts-and-diagrams.md) | SVG by hand, d3, Vega-Lite and Graphviz |
| [DuckDB](examples/04-duckdb.md) | SQL in the notebook with DuckDB compiled to WebAssembly |
| [Generative Art](examples/05-generative-art.md) | Seeded randomness, canvas and SVG, animation |
| [Widgets](examples/06-widgets.md) | Sliders and buttons that talk to the kernel |
| [UI Frameworks](examples/07-ui-frameworks.md) | React, Preact and Lit components as output |
| [Device](examples/08-device.md) | The `device` API: clipboard, haptics, dialogs, share and files |

The pages are built from the notebooks in
[ganymede-ui's examples/](https://github.com/tlockney/ganymede-ui/tree/main/examples),
as ganymede-ui last saved them. A widget needs a running kernel, so its
page says so instead of showing it, and the Device notebook has no
outputs, since its calls need the app.
