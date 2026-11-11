## 2026-11-11, landing page and README lead with the apps (#689)

The landing page and README now follow the order the docs plan in #689
set: the apps, then notebooks as programs, agent journals, TypeScript,
and one "Works with Jupyter" section. The hero shows the Mac app instead
of JupyterLab, its buttons are the Mac download and the command-line
install, and the install tabs moved under "Works with Jupyter", since
the command line is now the second way in.

"No Python runtime" is no longer a lead feature: it is one sentence in
the Jupyter section and in the README's, where it matters to someone
replacing `jupyter_server`. The six-feature grid and the "Get started"
reading-level cards are gone; the docs sidebar groups pages by job now,
so the landing page links straight to the guide for each section. The
frontends table went too, as the diagram and the Frontends card say the
same thing. Their styles (`.rt`, `.paths`, `.lvl`) went with them.

Every feature the page names is in the latest releases (ganymede 0.9.0,
ganymede-ui 0.8.0), so it needs no Unreleased note. The page doesn't
link the MCP tools reference yet; that page is still in review (#706).
