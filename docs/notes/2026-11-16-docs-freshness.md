## 2026-11-16, docs freshness: roadmap and live-agent-edits (#839)

The weekly docs check found the roadmap behind what merged since
2026-11-09. "Next" still listed notebook widgets (ganymede-ui#495), how
the agent learns of a person's edits, and tables, charts and SQL for
TypeScript notebooks, all of which shipped in the 0.11.0 releases; they
move to "Done". The Tailscale entry in "Open" said the identity in rooms
and the log, the apps and Tailscale SSH were next; they merged after the
releases, so the entry says what is left (an embedded tailnet node) and
moves below the list with an Unreleased note for the server and one for the apps. Item 4
has no "Next" now; that is Thomas's call.

`live-agent-edits.md` said "Nothing is built yet"; it now points to
Agents over MCP and the roadmap for what was built, since status lives
in the roadmap, and its "What happens today" section is dated. The `kernel.json` example in `install.md` showed
cdp-kernel 0.11.0; it shows 0.12.2, the pinned and latest version.
