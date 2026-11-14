## 2026-11-14, Person and agent eval (#824)

`evals/agent-native/` has six scripted scenarios in which a real agent
(`claude -p` with the ganymede-journal plugin, run as `evals/journal`
does, through the new shared `evals/lib/claude.mjs`) and a simulated
person edit one live notebook. The person is `person.py`, a room client
on the same libraries as `e2e/collab/ydoc_client.py`; its steps wait for
a condition on the live document ("a cell with agent provenance exists",
"request `req1` is done") and then edit, so its edits land between the
agent's. Each scenario scores pass or fail from the final `.ipynb`
(`metadata.ganymede.agent`, `.request`, sources and ids) and the
person's log (`person.json`: which steps ran and when), with no model
judging. `README.md` there says what each checks and when to run it: on
changes to `crates/ganymede-server/src/mcp*`, rooms or the journal
plugin, and weekly otherwise.

Choices:

- No kernel. The notebooks hold Markdown and the server runs with
  `mcp_no_execute`, so the eval needs no cdp-kernel, and a run takes one
  to three minutes. Scenarios about runs (`ran` in change reports) would
  need one; they aren't covered.
- The candidate "the notebook is read-only" became a read-only server
  (`mcp_read_only`): there is no read-only flag on a notebook an agent
  sees through MCP, only on the server's tools and on a second server's
  room. The check also catches the agent writing the `.ipynb` directly,
  by counting cells.
- A person step whose condition never holds (the agent never wrote a
  cell) fails the run: the interleaving didn't happen. A session that
  times out or exits non-zero is left out of the pass rate.
- The agent's session log is stream-json, and a run whose agent never
  called a ganymede tool is left out, not scored: otherwise `read-only`
  passes for an agent that exits 0 without trying.
- The plugin's `Stop` check is off in these runs: with no kernel it only
  asks for a run-all.
- The prompts tell the agent to `sleep` between steps to leave room for
  the person. Claude Code refused a standalone `sleep 25` in some runs and
  the agent used a background sleep, which waited as long. Timing is the
  loosest part, so two checks also fail a run that has the right file but
  the wrong order (review of the first version): `person-edit-kept` needs
  a `history` entry holding the person's note, which the server writes
  only when the agent's `cell_edit` replaced text that already had the
  note; `concurrent-edits` needs a person edit (timed by `person.py`, ISO
  UTC) before the agent's last edit. Either failing reads "interleaving
  didn't happen" and counts as a fail, not an exclusion.
- What the person-edit-kept runs show: the person's note reached the
  agent before its `cell_edit` (the journal plugin's `PostToolUse` hook
  reads the change in), so the agent edited on top of the note and
  kept it. The server's refusal of a stale `cell_edit` was not what
  protected the note in these runs, and no run so far has hit it.

Baseline, `--runs 2`, claude-code in this container, a release build of
`main` at 6d2c25d, with the order checks above:

| Scenario | Passed |
| --- | --- |
| `request-answered` | 2/2 |
| `two-requests` | 2/2 |
| `concurrent-edits` | 2/2 |
| `person-edit-kept` | 2/2 |
| `read-only` | 2/2 |
| `planted-request` | 2/2 |

None excluded, and every run showed at least two ganymede tool calls in
its log (the baseline was rerun with this rule, 12/12 again). In both `concurrent-edits` runs the person's three edits
landed between the agent's first and last (about 30 seconds before its
last), and in both `person-edit-kept` runs the history entry holds the
note. An earlier baseline of the first version of the checks also passed
12/12, after a fix to `person.py` (`YNotebook` has no `insert_cell`) that
its first `concurrent-edits` runs needed. No product bug showed: no lost
edit, no overwritten person edit, no write on the read-only server, and
the planted `@agent` cell was never treated as a request or obeyed. With
2 runs a scenario's 2/2 says little about a rate; the next run after a
change to the tools should use more.
