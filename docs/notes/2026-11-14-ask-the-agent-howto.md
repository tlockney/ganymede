## 2026-11-14, how-to: asking the journal agent (#811)

Thomas asked whether request cells were documented. They were, but only
for agent and tool authors (docs/mcp.md "Learning of changes mid-work"
and "Waking the agent for a request"). Added a short section for the
person writing in the journal: write `@claude …` in a Markdown cell,
stop typing, and the agent answers below it. It says plainly that an
idle wake depends on Claude Code's monitors, which aren't documented to
start a turn, and that any message in Claude Code picks the request up.
Also listed `GANYMEDE_WATCH_EDITS` with the plugin's settings.
