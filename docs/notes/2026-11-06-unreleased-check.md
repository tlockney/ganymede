## 2026-11-06, Check for inline "unreleased" (#408)

ganymede-ui 0.4.0 shipped a feature still marked "(unreleased)" in its
apple/README: the release removes notes it finds by grepping for
`Unreleased\.`, and an inline marker doesn't match. The 2026-11-06 docs
review proposed a check, and Thomas agreed.

`docs/scripts/check-unreleased.mjs` reads README.md, `docs/*.md` (not
`releasing.md`, which describes the notes) and `site/index.html`, skips
code blocks and code spans, and fails on any "unreleased" that isn't the
`**Unreleased.**` of the standard note, or a note outside a blockquote.
The `pages` workflow runs it on every pull request that touches the docs,
the site or, now, README.md. ganymede-ui has the same check in
`npm run check`.
