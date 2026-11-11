## 2026-11-11, the apps' example notebooks on the site (#689)

The eight notebooks the apps copy with **Open Example Notebooks** now
have pages under /docs/examples/, with an index at examples.md and an
Examples group and nav entry. `npm run fetch-examples`
(docs/scripts/fetch-examples.mjs) renders them from ganymede-ui's
`examples/*.ipynb` on its main, like `fetch-ui` and `fetch-cell-api`,
so the pages aren't committed and follow ganymede-ui as it re-runs them.

Each page shows the code cells and the outputs the notebook saved:
Markdown, text, JSON and errors as they are, SVG and bitmaps as image
files beside the page, HTML in place. HTML with a `<script>` (the React,
Preact and Lit outputs) runs in an iframe with `sandbox="allow-scripts"`
and no `allow-same-origin`, so it gets an opaque origin and can't touch
the docs page; those scripts load their framework from esm.sh in the
reader's browser, as they do in the app. A widget needs a kernel, so its
page says so. Outputs keep a light panel in the dark theme, since they
were drawn for a light notebook.

Fourth item of #689, first half; the Fremont and Oklahoma journals come
next, re-run on the current release.

The example pages' names follow ganymede-ui's notebook file names, so
fetch-examples also writes docs/examples/pages.json and config.mts builds
the sidebar's Examples group from it; a link to an example page that
wasn't rendered warns instead of failing the build, as links into
ganymede-ui's docs do. HTML with a `<style>`, `<link>`, `<pre>`, an
inline `on…=` handler or another embed goes to the sandboxed frame too.
