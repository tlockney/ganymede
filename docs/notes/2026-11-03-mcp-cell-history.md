## 2026-11-03, `cell_edit` keeps what it replaced (#314)

Journaling revises a wrong finding where it stands instead of appending
a correction (#311, the Ganymede Journal design). To keep that honest,
`cell_edit` now appends the replaced source to
`metadata.ganymede.agent.history` as `{"source", "by", "at"}`.

- Bound: the last 10 entries per cell (`mcp::HISTORY_LIMIT`). Count, not
  bytes: a finding is short, and a code cell large enough to matter is
  better kept in git than in metadata.
- An edit that leaves the source unchanged adds nothing, so `cell_edit`
  with `run: true` used only to re-run a cell leaves no noise.
- `by`/`at` on an entry name the edit that replaced the text, not the
  author of the old text. The cell's own `by` stays its first author,
  and its `at` moves to the latest edit, as before.
- nbformat allows any metadata, so JupyterLab and nbformat read these
  notebooks unchanged; the test reads the saved file back with
  `ganymede_nbformat`.
