## 2026-10-18, one file per log entry (#83)

Every PR appended to the end of `docs/notes.md`, so any two open PRs
conflicted there and each merge made the others resolve it. Entries are
now files in `docs/notes/`, named `YYYY-MM-DD-<topic>.md`; the log before
the split is `docs/notes/archive.md`, unchanged apart from its links.

- The site still has one log page. `docs/.vitepress/notes.ts` appends
  the entries to `notes.md` in a Vite plugin that runs before VitePress's
  Markdown plugin, so the outline and the dead-link check see
  the full page. `srcExclude` keeps the entries from becoming pages too.
- Local search does not go through Vite: it reads each page from disk.
  Its `_render` option gets the same treatment, or the entries would be
  missing from search.
- Entries link relative to `docs/notes/` so they work on GitHub; the
  plugin rebases those links to `docs/` for the page.
- A file in `docs/notes/` with another name fails the build, rather than
  silently dropping off the page.
- Entries on the same day sort by topic, not by merge order.
