## 2026-11-14, the journal plugin sets no version (#809)

`plugin.json` had set `"version": "0.1.0"` since #315, and none of the
18 plugin changes since bumped it. Claude Code computes an installed
plugin's version from the manifest first, then the marketplace entry,
then the source's commit SHA, and updates only when that version
changes. So `claude plugin update` and auto-update left everyone on the
copy they first installed: the `requests` monitor (#786) and the
request tools (#784) never arrived. Reproduced with an isolated `HOME`:
the install landed in `cache/ganymede/ganymede-journal/0.1.0`, and after
dropping `version` in the marketplace clone, `claude plugin update`
moved it to the commit SHA.

Two fixes were open: tie a version to Ganymede's release, or set none.
The plugin already installs from `main` and checks what the `ganymede`
on `PATH` supports (the monitor exits when `mcp watch` is missing), so
it now sets none and each commit to `main` is an update. A release-tied
version would hold plugin fixes back until a release and needs a bump
step that can be forgotten in the same way.

CI's `plugin` job fails when `plugin.json` or the marketplace entry
sets `version`. docs/mcp.md says how to update: turn on auto-update for
the `ganymede` marketplace (off by default for marketplaces other than
the official ones), or `claude plugin marketplace update ganymede` and
`claude plugin update ganymede-journal@ganymede`, then restart.
