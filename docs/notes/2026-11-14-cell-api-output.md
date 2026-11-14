## 2026-11-14, cell API pages show output and live widgets (#807)

cdp-kernel's reference pages now name a saved notebook and mark runnable
examples as ```` ```ts cell ```` (cdp-kernel#500, #502). `fetch-cell-api`
reads that notebook and puts each cell's outputs after its block, using
the example pages' output renderer (`fetch-examples.mjs`), so
/docs/cell-api/ shows what cdp-kernel.org shows. A block the notebook
doesn't have as written fails the build, as on cdp-kernel.org.

Widgets: an anywidget saved with its state and view module now runs on
the page from that state (`theme/widgets.ts`, copied from cdp-kernel's;
the two should stay in step). cdp-kernel's docs runner saves each widget
output's state as of its own cell in `metadata.docs`, and the page starts
from that, so a later cell's change doesn't show in an earlier output.
The example pages keep the "needs a running kernel" note: their notebooks
carry no snapshots, so there is no state to start a widget from.

`text/plain` outputs now drop terminal colours, as streams and errors
already did: cdp-kernel's inspected values carry them.
