## 2026-10-18, one Install page (#150)

Install and Install on a workstation covered the same path: the install
script, the `fake` check, cdp-kernel, and the macOS and glibc notes
appeared on both. The workstation page's numbered steps are now
`docs/install.md`, with the old Install page's own parts (script options,
by hand, verifying a download, from source) in a closing "Other ways to
install" section. Their anchors (`#by-hand`, `#from-source`,
`#verify-a-download`) are unchanged, since other pages and cdp-kernel link
them.

- `docs/workstation.md` stays as a stub: on the site it redirects to
  `/docs/install` with the hash, so `/docs/workstation#_5-start-jupyterlab`
  lands on the same step (the numbered headings kept their text, so their
  slugs match). On GitHub it is a one-line link. `search: false` keeps it
  out of the search index.
- VitePress prefixes a slug that starts with a digit with `_`
  (`#_9-troubleshooting`); GitHub doesn't (`#9-troubleshooting`). A link
  that has to work in both places can't target a numbered heading, so
  known-gaps.md links the page instead.
