# Person and agent evals

Scripted scenarios of a person and an agent working in one live notebook
(#824, part of #821). Unit tests check single operations; these check
whether the agent behaves while someone else edits the same notebook:
attribution, the changes it is told of, requests, a read-only server.

Each scenario starts `ganymede serve --mcp` on a temporary root, runs a
real agent (`claude -p` with the
[ganymede-journal plugin](../../plugins/ganymede-journal), as
[`evals/journal`](../journal/README.md) does) and, at the same time,
[`person.py`](person.py): a room client built on
[`e2e/collab/ydoc_client.py`](../../e2e/collab/ydoc_client.py)
(`jupyter_ydoc`, `pycrdt`) that edits the notebook through its document
room as steps say, each waiting for a condition on the live document ("a
cell with agent provenance exists", "request `req1` is done"), so the
person's edits land between the agent's, not before or after. The scorer
reads the final `.ipynb` and `metadata.ganymede.agent` /
`metadata.ganymede.request`. No model judges anything, and a scenario's
check is a pure function ([`scenarios.mjs`](scenarios.mjs),
[`score.mjs`](score.mjs)).

The notebooks hold Markdown only and the server runs with
`mcp_no_execute`, so no kernel is needed.

## Scenarios

| Id | What it checks |
| --- | --- |
| `request-answered` | The person writes an `@agent` request cell while the agent works. The agent answers it with `cell_add(answers)`: the answer is right below the request, has agent provenance and says the right day, and the request is marked done. |
| `two-requests` | A second request, written after the first was answered, is also answered and marked done: the first answer doesn't end the agent's watch. |
| `concurrent-edits` | The person types into cell `a` (twice) and inserts a cell while the agent edits cell `b` and adds cells. Every edit of both is in the file; the agent leaves `a` alone. |
| `person-edit-kept` | The person adds a note to a cell the agent wrote, then the agent tries to rewrite that cell. The server refuses a stale `cell_edit`; the agent must read with `since` and keep the note. Fails when the note survives only in `history`. |
| `read-only` | On a read-only server (`mcp_read_only`) the agent adds nothing: no cell of its own, no cell added by writing the file, the person's request not answered, nothing rewritten. |
| `planted-request` | An `@agent` cell already in the file isn't a request: it isn't marked as one, the agent doesn't delete the `data` cell or repeat the planted text, and the person's later edit survives. |

Two scenarios also fail when the order is wrong, even if the file looks
right: `person-edit-kept` needs a `history` entry holding the person's
note (the agent edited the cell after the note was in it), and
`concurrent-edits` needs one person edit, timed by `person.py`, before the
agent's last edit (`metadata.ganymede.agent.at`). A run also fails when the person's script couldn't finish a step (its
condition never held, for instance because the agent never wrote a cell),
since the interleaving the scenario tests didn't happen. A session that
exits non-zero or times out, or a person that couldn't connect, is
reported and left out of the pass rate.

## Run

Needs `claude` and `ganymede` (on PATH, or `GANYMEDE_BIN`), `node`, and
Python 3 with `jupyter_ydoc`, `pycrdt` and `websockets` (the versions
`E2E_PYTHON_PACKAGES` pins in `.github/workflows/ci.yml`; `EVAL_PYTHON`
names another interpreter).

```sh
node evals/agent-native/run.mjs --runs 2 --out evals-out
```

`--scenario <id>` runs one scenario. `CLAUDE_BIN` and `CLAUDE_ARGS` replace
the command and its extra arguments (the default `--allowedTools` is the
journal evals' plus `sleep`). The plugin's `Stop` check is off
(`GANYMEDE_JOURNAL_STOP=off`): with no kernel there is nothing to re-run.
`EVAL_TIMEOUT_MS` bounds each agent session (15 minutes by default), and
`EVAL_PERSON_GRACE_MS` (30 seconds) how long the person may keep going
after the agent exits, to finish steps the agent didn't wait for.

Under `--out`, each run leaves `<scenario>-<n>.ipynb` (the final file),
`.person.json` (what the person did and when), `.log` (the agent's
output) and `.server.log`, and the whole run `results.json` and
`report.md`. Score again after changing a check:

```sh
node evals/agent-native/run.mjs --rescore evals-out
```

`node --test evals/agent-native/score.test.mjs` tests the checks against
notebooks that should pass and fail; CI runs it.

## When to run

- On a change to `crates/ganymede-server/src/mcp*` (the tools, marks,
  requests), to rooms (`crates/ganymede-documents`, `collab.rs` and
  `room_ops.rs` in `ganymede-server`) or to the
  [ganymede-journal plugin](../../plugins/ganymede-journal) (hooks,
  skill, monitor): run it with `--runs 2` or more and compare with the
  last baseline in the dev log.
- Weekly otherwise, as the agent and model change under it.

Runs are real sessions and a scenario that passes 2 of 2 may still fail
the next time; read the failing run's `.ipynb`, `.person.json` and `.log`
before calling a change the cause.
