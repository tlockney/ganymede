## 2026-11-07, release.sh lists only real Unreleased notes (#491)

`scripts/release.sh --execute` lists the "Unreleased" notes for the
release pull request with a plain `grep 'Unreleased\.'`, which also
matched the note's template in a fenced block of `docs/development.md`
and the prose after it, so every release listed two lines that weren't
notes.

In Markdown it now counts only `> **Unreleased.**` lines outside fenced
blocks (an awk pass that tracks ``` and ~~~ fences per file). The
landing page's notes have no fixed form (one was a comment in a code
sample), so in `site/index.html` any "Unreleased." still counts.
