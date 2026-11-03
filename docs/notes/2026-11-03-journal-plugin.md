## 2026-11-03, The ganymede-journal plugin (#315)

`plugins/ganymede-journal/` is a Claude Code plugin, listed in this
repo's `.claude-plugin/marketplace.json` (marketplace `ganymede`): the
`ganymede mcp` server, a `journal` skill, a `journal-reviewer` agent,
and four hooks in one Node script. Built on #313's read marks.

- Hooks need the server the agent uses, from another process. A shared
  `ganymede serve --mcp` is found by its `jpserver-*.json`, but the
  server `ganymede mcp` starts for itself was unreachable. It now
  writes `ganymede-mcp-<pid>.json` (url, token, root) while it runs,
  which only `ganymede mcp call` reads: another `ganymede mcp` must not
  share a server that stops with its agent. `ganymede mcp call TOOL
  ARGS` opens a session, calls one tool, prints its text, ends the
  session.
- A hook's edit comes from its own MCP connection, so it would show up
  in the agent's next `notebook_read(since)` as someone else's.
  `cell_add` takes `mark`, the agent's read mark, and moves it past the
  new cell, as the agent's own edits do.
- Answers to the issue's open questions. The plugin lives in this repo,
  next to the server it drives, so a PR changes both together. The Bash
  hook records only commands ending in `# journal`
  (`GANYMEDE_JOURNAL_RECORD=all` records every one): recording every
  call would fill a journal with `ls` and builds. Recorded cells are
  Markdown with fenced blocks, which renders everywhere and matches
  `cell_add(mode: recorded)`.
- `Stop` fires at the end of every turn, not only at "done". Blocking
  every turn while the journal is dirty would force a restart-and-run-all
  before each question to the user, so it blocks once per round of
  edits and the reason lets the agent say it is only pausing.
- Plugin skills are named `/<plugin>:<skill>`, so the skill is
  `/ganymede-journal:journal`, not the issue's `/ganymede:journal`, which
  would need the plugin itself named `ganymede`.
- MCP tools from a plugin are `mcp__plugin_<plugin>_<server>__<tool>`;
  hook matchers and the reviewer's `tools` must use that full name. The
  hooks also match `mcp__ganymede__*`, for `claude mcp add ganymede`.
- Hooks are plain `.mjs`, not `.ts`: Node strips types only from 22.18
  and 23.6, and a hook must run on whatever `node` is on `PATH`.
- `claude plugin validate --strict` passes for the plugin and the
  marketplace (it warns about the missing icon), and
  `claude plugin install ganymede-journal@ganymede` installs it from a
  local checkout. The hook tests run in CI against this build's
  `ganymede mcp`.
