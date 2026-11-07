## 2026-11-07, release.sh lists only real Unreleased notes (#491)

`scripts/release.sh --execute` lists the "Unreleased" notes for the
release pull request with a plain `grep 'Unreleased\.'`, which also
matched the note's template in a fenced block of `docs/development.md`
and the prose after it, so every release listed two lines that weren't
notes.

In Markdown it now counts what `docs/scripts/check-unreleased.mjs`
(#408) accepts as a note: a line starting with `>` (indented or not)
that holds `**Unreleased.**` outside fenced blocks and code spans. An
awk pass tracks ``` and ~~~ fences per file, as the CI check does. A
stricter rule would miss notes CI lets through, such as one indented in
a list item, and the release would ship them. The
landing page's notes have no fixed form (one was a comment in a code
sample), so in `site/index.html` any "Unreleased." still counts.
