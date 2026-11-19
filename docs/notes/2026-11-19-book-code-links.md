## 2026-11-19, the book links to the code (#873)

File and crate names in the book (`site/book/index.html`) now link to
the file, or the crate's folder, on `main` on GitHub: 368 links across
the three repositories.

What to know:
- The generator resolves each name against the repositories' file trees.
  A name links only when one file fits. It narrows the candidates by the
  crate the text named most recently, then the chapter's own crate, then
  the chapter's repository, and passes over tests, e2e suites and spikes
  unless nothing else matches. A few names it would get wrong are set by
  hand (for example `ci.yml` in chapter 1 is ganymede-ui's), and names
  the text gives for every repo ("`scripts/ci-changes.sh` beside it")
  stay plain text, as do patterns like `kernel-<id>.json`.
- Links point at `main`, not a commit, so they show the code as it is
  now. A file renamed later breaks its link until the book is rebuilt.
  The weekly docs check rebuilds the book when its text drifts, which
  also refreshes the links.
