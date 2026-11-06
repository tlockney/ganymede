# Journaling evals

A small set of explorations with known answers (#316), run headless with
`claude -p` and the [ganymede-journal plugin](../../plugins/ganymede-journal),
to tell whether a change to the tools, the skill or the plugin makes
journals better.

## Run

Needs `claude`, `ganymede` on PATH (or `GANYMEDE_BIN`), and the
`cdp-node` kernel the skill defaults to.

```sh
node evals/journal/run.mjs --runs 3 --out evals-out
```

`--task <id>` runs one task. `CLAUDE_BIN` and `CLAUDE_ARGS` replace the
command and its extra arguments (the default `--allowedTools` allows the
ganymede tools, the file tools and `grep`, `wc`, `head`, `ls`). A session
that exits non-zero, or runs past `EVAL_TIMEOUT_MS` (30 minutes by
default), is reported and left out of the measures.

Each session leaves `<task>-<n>.end.ipynb`, kept as the session left it,
and `<task>-<n>.ipynb`, for a person to read and edit. After editing,
score again:

```sh
node evals/journal/run.mjs --rescore evals-out
```

## Tasks

[`tasks.mjs`](tasks.mjs) builds each task's files from a seeded
generator, so every run gets the same data and answers. Each has a trap:

- `sales-best-month`: some March orders appear twice; deduplicated, a
  different month leads.
- `log-errors-by-hour`: asks for a `grep` in the shell first, which
  belongs in a recorded cell.
- `warmest-station`: one station reports Fahrenheit.

## Measures

[`score.mjs`](score.mjs) reads only the `.ipynb`:

| Measure | What it counts |
| --- | --- |
| `cleanRunAll` | Share of sessions whose kernel cells ran 1..n with no errors, as `notebook_run_all` leaves them |
| `passedOff` | Cells showing output the kernel didn't produce: outputs on a recorded or note cell, or on a code cell with no execution count. Should be 0 |
| `recordedWhenNeeded` | Share of tasks that ask for a shell step with at least one recorded cell |
| `revisedInPlace` | Earlier versions kept in Markdown cells' `history` |
| `appendedCorrections` | Markdown cells that read as a correction ("Correction:", "Actually", ...) |
| `summaryRewritten` | Share of reviewed sessions where the person changed the summary (the first numbered list after the title) |
| `answered` | Share of sessions whose Markdown states the task's answer |

`node --test evals/journal/score.test.mjs` tests the scoring; CI runs it.
