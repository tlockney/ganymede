## 2026-11-13, design note: an agent that responds to your edits (#761)

Thomas asked whether the journal agent responds to edits he makes in the
journal, and how rooms could make it. Reading the MCP server and the
plugin's hooks: the agent learns of a person's edits only when the
person next writes to it, through the `UserPromptSubmit` hook and the
read marks from #313. Mid-turn and idle it learns nothing, changes carry
no author, re-runs aren't compared, and `cell_edit` replaces a cell with
no check, so it can overwrite an edit it hasn't seen.

The note, [An agent that responds to your edits](../live-agent-edits.md),
proposes per-cell attribution kept in memory, runs reported as changes,
a one-line `@@changed` pointer on every tool reply, an `@agent` request
cell, and a check that refuses an edit to a cell changed since the
agent's mark. For waking an idle agent it compares a plugin monitor
running a new `ganymede mcp watch`, a Claude Code channel (research
preview, allowlisted only) and MCP resource subscriptions (Claude Code
documents no use of them). Four decisions are left for Thomas.
