## 2026-11-09, Cell API reference on the site (#607)

The reference pages for what cells call, generated from TSDoc in
cdp-kernel (`jupyter`, widgets and comms, tlockney/cdp-kernel#441) and
ganymede-device (`device`, tlockney/ganymede-device#88), now show on the
site under "Cell APIs" (/docs/cell-api/). Like ganymede-ui's docs, they
are fetched at build time (`npm run fetch-cell-api`), not committed. The
fetch renames the pages (`jupyter-reference.md` to `jupyter.md`, and so
on) and rewrites their links: between the pages, to cdp-kernel.org for
cdp-kernel's guides, to this site for ganymede-ui's `device` guide, and to
GitHub for anything else, so a new link upstream can't fail this build.
"Edit this page" points at the doc comments' source. Next on #596: bundle
the pages in the apps.
