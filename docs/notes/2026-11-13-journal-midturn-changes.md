## 2026-11-13, journal plugin passes changes mid-turn (#779)

Part of #763. The journal plugin's `PostToolUse` hook now looks for
`@@changed` lines in a ganymede tool reply, makes the
`notebook_read(path, since)` call each names, and adds what changed as
context right after the reply, as the `UserPromptSubmit` hook does when
the person writes. The read moves the agent's mark, so the next reply
doesn't repeat the line. The skill says the changes then need no read
of their own. Bash replies aren't checked, so a person's edit reaches
the agent on its next ganymede tool call, not between shell commands.
