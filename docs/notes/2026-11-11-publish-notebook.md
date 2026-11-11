## 2026-11-11, Publishing a notebook as a page (#677)

Thomas asked for a journal to be publishable as a page worth reading, as
part of the journal plugin. The two worked examples (Fremont Bridge,
Oklahoma quakes) had their pages built by hand, and their notebook
renders were DOM snapshots of ganymede-ui taken with Playwright. Neither
was something a tool could repeat.

The exporter sits in the core, in `ganymede-nbformat` beside the Markdown
and script exporters, so every entry point gets the same page:
`ganymede_nbformat::to_html`, `GET /nbconvert/html/{path}` (nbconvert's
`html` format, so JupyterLab's export menu offers it), `ganymede nb
publish`, and the MCP tool `notebook_publish`, which saves the page under
the root (`published/<name>.html` beside the notebook by default). The
journal skill gained a Publish step: finish the journal, publish, and,
when the agent has a tool that hosts pages (a Claude artifact), host that
file unchanged. "Artifact" here means the file. The server can't reach
claude.ai, and one self-contained HTML file covers both cases.

The page uses the site's palette: warm paper, teal accent, serif prose,
system fonts only. It has a contents column on wide screens and follows
the reader's light or dark setting. Charts sit on a light card in both
modes, since Plot draws in `currentColor`, which in an `<img>` is black.
Code is folded above each result by default (`--code shown|hidden`).
In a journal, cells without agent provenance are marked as a person's,
and recorded steps are set apart. The header names who kept the journal.

What the page may hold was the main design point:

- Only cell sources and outputs. Metadata stays out: the provenance
  history (which keeps replaced text, sometimes on purpose a secret's
  removal), cell ids and signatures. Only the kernel's display name and
  the agents' names appear.
- The server replaces its root (both spellings, since macOS resolves
  `/var` to `/private/var`), the home directory and its token. The CLI
  replaces the home directory and the notebook's directory. The skill
  tells the agent to read the notebook for other private values before
  hosting the page, since no list of patterns catches them all.
- No network and no notebook script. SVG goes in as a `data:` image, so
  its scripts don't run. This needed the SVG namespace added: Plot
  serializes SVG from an HTML DOM without `xmlns`, and an `<img>` draws
  nothing without it. HTML output goes in an `<iframe sandbox
  srcdoc>` without `allow-scripts`; `allow-same-origin` lets the page's
  one script size the frame. Raw HTML in Markdown is escaped, links keep
  only `http`, `https`, `mailto` and relative targets, and images other
  than `data:` ones become links. Outputs that need JavaScript
  (Vega-Lite, widgets) fall back to another bundle entry or a note.

New dependency: `pulldown-cmark` 0.13 (MIT), default features off, for
Markdown. Nothing in the tree rendered Markdown to HTML, and a
hand-written renderer would have been the larger risk. Image outputs come
before HTML in the page's priority order, unlike nbconvert's, because an
image needs no frame.

Served from `/nbconvert/html/`, the page runs under the files CSP
(`sandbox allow-scripts`), whose opaque origin stops the frame-sizing
script. HTML outputs then show at a fixed height with a scrollbar. The
downloaded or hosted file is not affected.

The independent review found that the frames stopped scripts but not
fetches: an HTML output's `<img src="https://…">` or stylesheet still
loaded in every reader's browser. Each frame's document now starts with
a `Content-Security-Policy` meta (`default-src 'none'; img-src data:;
style-src 'unsafe-inline'`), ahead of the output, so a policy the output
adds can only narrow it. The page itself has no policy, since a host such
as the artifact viewer may add its own scripts. Redaction now matches
whole names only, and skips a root such as `/` (a notebook directory or
server root of `/` had turned every `/` into `.`). The CLI and server
share one helper, so both redact each directory as given and resolved.
`notebook_publish` replaces only an earlier published page, recognized by
its generator tag, unless `overwrite: true`. The title is parsed as
inline text, so `# 1. Setup` keeps its number. Footnotes are off, since
each cell renders alone and their ids would collide. A raw `<img>` that
shows an attachment becomes an image tag of the page's own making.
