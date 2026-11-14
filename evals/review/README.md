# Review replay eval

A regression set for the pre-push review prompt
(`reviews/pr-reviewer/review-prompt.md`, #822, part of #821). Reviews vary
between runs, so this gives a number for how much the prompt catches and a
way to see whether an edit to it helps.

Each **case** is a diff a reviewer saw (repo, base sha, head sha, issue)
with the known Medium or High finding it should flag, and that finding's
class (A to I, from the 2026-11-14 review-patterns analysis). A **run**
gives the prompt, word for word, plus the trailer the workflow adds, to
`claude -p` on Opus, read-only. A **judge** (Sonnet) gets the known
findings and the review's text and scores each `caught`, `partly` or
`missed`, and counts any other Medium-or-worse findings.

## Classes

| Class | What it is |
| --- | --- |
| A | Concurrency, ordering, lifecycle (races, queued ops, cancel, restart, state keyed by a reused id) |
| B | Security boundaries (auth, origin, sandbox, path containment, grants that widen, secret scope) |
| C | Not every entry point or consumer covered |
| D | Docs and process |
| E | Tests that don't prove the claim (stand-ins that differ from the real library) |
| F | Errors swallowed or replaced |
| G | Platform semantics |
| H | Version skew and release wiring |
| I | Edge-case input |

## Cases

[`cases/`](cases) holds one JSON file per case, `<repo>-<pr>.json`:

| Field | Meaning |
| --- | --- |
| `id` | The file name without `.json` |
| `repo`, `pr`, `issue` | `tlockney/<repo>`, the PR, the issue it resolves (passed as `ISSUE`) |
| `base`, `head` | Full shas. `base` is what the reviewer diffed against (the merge base; for a stacked PR the base branch's tip) |
| `branch` | The PR's head branch name, passed as `BRANCH` (`eval/<id>` when absent) |
| `kind` | `known`: a posted review's first round found it. `missed`: a passing review lacked it and a later run found it. `clean`: passed with nothing to fix |
| `findings` | `{summary, severity (Medium or High), class (A to I), source}`; empty for `clean` |
| `notes` | Anything that explains the choice of head or base |

The set today: 10 `known`, 6 `missed`, 5 `clean`. For a `missed` case the
head is the one the passing review was posted on, except where the code
carrying the finding came in a later commit (the notes say so).

### Adding a case

Every new entry under "Gaps found / changelog" in `review-prompt.md` adds
its case, in the same change. One JSON file:

1. Find the PR head the review saw (the `Reviewed commit <sha>` line, or
   the review's `commit_id`) and its base (`git merge-base <head> main`,
   or the base branch's tip for a stacked PR). Use full 40-character shas.
   A sha that a force-push removed can still be fetched by sha.
2. Write the finding as a defect, in a sentence or three, with the failing
   scenario. Do not copy the prompt's wording of the check that was added
   for it: the case measures whether the review finds the defect, and the
   judge compares meaning.
3. Pick the class and the kind. Copy a neighbour as a template.
4. `node --test evals/review/*.test.mjs` validates every case file.
5. Run it: `node evals/review/run.mjs --prompt PATH --cases <id> --out DIR`.
   A case that the current prompt already catches twice is fine (it guards
   against regressions); one that needs the new check should be missed
   before the edit and caught after.

A clean case is a merged PR whose first review on its final head said
`Review: pass`, with no later fix PR, issue or commit naming it, and no
findings file in `reviews/pr-reviewer/`. Keep them small to medium.

## Run

Needs `claude` and `git` on PATH, network access to GitHub for the first
clone, and a Claude Code login.

```sh
node evals/review/run.mjs --prompt PATH/review-prompt.md --runs 2 --out out
node evals/review/run.mjs --prompt PATH --lens security --out out        # lens runs, A/B cases
node evals/review/run.mjs --prompt PATH --lens concurrency --out out
node evals/review/run.mjs --report out                                   # rebuild the report
node --test evals/review/*.test.mjs
```

Options: `--cases IDS` (comma-separated ids, `*` wildcards), `--runs N`
(default 2; 1 with `--lens`), `--model` (default `opus`),
`--judge-model` (`sonnet`), `--concurrency` (2), `--cache DIR` (bare
clones and scratch checkouts, default `~/.cache/ganymede-review-eval`, or
`REVIEW_EVAL_CACHE`), `--redo` (run again what `results.json` already
holds). Runs finished in `--out` are kept, so an interrupted run resumes
and lens runs add to the same report. `EVAL_TIMEOUT_MS` limits a session
(default one hour); `CLAUDE_BIN` replaces the command. A run that exits
non-zero is retried once, and a run that still fails is listed in the
report, not scored.

To compare a draft edit, point `--prompt` at the draft and `--out` at a new
directory, then compare the two `report.md` files.

What a run does:

- `--lens NAME` adds one line after the prompt text, before the trailer:
  `Lens for this run: look hardest at security boundaries.` (or
  `concurrency and lifecycle`). Nothing else changes.
- The review sees a scratch clone at the case's head, with the branch at
  `head` and `origin/main` at `base`, so `git diff origin/main...HEAD` is
  the case's diff and nothing after it is reachable from a ref. The trailer
  is `REPO` (and clone path), `BRANCH`, `ISSUE` and `DIFF`, and nothing
  more, no steering.
- Tools: `Read`, `Grep`, `Glob`, `Skill`, subagents, read-only `git`
  commands, `gh issue view` (the issue text) and a few shell readers. No
  edits, builds or tests, and no `gh api` or `gh pr`, which would show the
  posted review.

### Limits

The reviews cannot build or run tests, which the prompt allows in life, so
the measure is the review's reading of the diff. Class E findings (tests
that don't prove the claim) lose the most. The reviewer reads the issue
with `gh issue view`, not the GitHub MCP tool the prompt names. The judge
is a model: read a few reviews and verdicts by hand after each prompt
change (`reviews/<case>-<run>.md`, `results.json`) before trusting a
small difference. With about 30 findings and two runs, a swing of a few
points is noise.

## Measures

[`score.mjs`](score.mjs) holds the pure parts (tested in
[`score.test.mjs`](score.test.mjs)):

| Measure | What it counts |
| --- | --- |
| Single-run recall | (caught + 0.5 * partly) over every known finding in every run |
| Any-run recall | The same, taking the best of the runs for each finding: what a second run adds |
| By class, by kind | The same, grouped |
| False alarms | Other Medium-or-worse findings per run on `clean` cases, runs with at least one, and runs ending `Review: changes needed` |
| Two lenses vs one run | On the class A and B findings: one plain run, the best of two plain runs, and the best of a security-lens run and a concurrency-lens run, with cost. The lens pair is justified when it beats one run by 15 points and beats two plain runs |

`out/report.md` has the tables, `out/summary.json` the numbers, `out/reviews/`
each review's text, `out/results.json` each run's judge output, cost and tokens.

## When to run

- On each change to `review-prompt.md` (a new Gaps entry, a tightened
  check), against the prompt before the change.
- Monthly, to see drift (a new model, new skills, new defaults).

## Cost

About $1 to $2 of Opus per review, 4 to 10 minutes each (about $1.15 and
2.5 minutes for the smallest), plus a few cents for each judge call. The
whole set at 2 runs (42 reviews) is roughly $60 to $80; the lens runs add
about 12 reviews. The baseline is in
[`baseline-2026-11-14.md`](baseline-2026-11-14.md), with its actual cost.
