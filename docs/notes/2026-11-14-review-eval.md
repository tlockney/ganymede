## 2026-11-14, Review replay eval (#822)

`evals/review/` replays the pre-push review prompt on 21 cases and scores
what it catches (part of #821). A case is a diff a reviewer saw (repo, base
sha, head sha, issue) with the known Medium or High finding and its class A
to I from the review-patterns analysis. Ten cases are first-round diffs of
posted `changes needed` reviews, six are diffs a passing review missed and a
later run found, five are clean diffs that passed with no follow-up. The
runner gives `review-prompt.md` word for word plus the workflow's trailer
(REPO, BRANCH, ISSUE, DIFF) to `claude -p` on Opus with read-only tools, in a
scratch clone whose `origin/main` is the case's base and whose objects
hold only the history of base and head, so no later commit can be read by
sha. A Sonnet judge scores each review caught, partly or
missed per finding and counts other Medium+ findings. The scoring is pure and
tested (`node --test evals/review/*.test.mjs`, in the `plugin` CI job). How to
run it and how to add a case (every new Gaps entry adds its case) is in
`evals/review/README.md`.

Clean cases were picked from merged PRs in ganymede, ganymede-ui and
cdp-kernel with 60 to 700 changed lines, at most 10 files, based on `main`,
whose only review was one `Review: pass` on the head that merged. Excluded:
releases and docs-only PRs, PRs in a findings file under
`reviews/pr-reviewer/`, and PRs another PR or issue names (searched with
`gh api search/issues` for `#N` and `repo#N`). Left are ganymede #787,
ganymede-ui #601 and #541, cdp-kernel #439 and #416.

The first baseline (total $60.71) was discarded after the pre-push review
found flaws in the runner: reviews could not read the issue (55 of 62 said so:
`gh issue view` needs GraphQL, which these sessions can't reach, and a review
without the GitHub MCP tool the prompt names never thought of `gh api`); read
tools were unscoped, so a review could read the answer files under
`/mnt/project-files`, other clones and the eval's own output; the scratch
clone shared objects with the bare cache, so later commits were readable by
sha; `find`, `git grep` and `git log --output` could run commands or write.
The runner now scopes reads to the clone and denies those paths, serves the
case's own issue through a local `github` MCP server (`issue_read`, as the
prompt says) plus an exact `gh api` rule, fetches only base and head into the
clone, and drops the write-capable commands. A probe run confirmed each
denial. It also refuses to resume an `--out` made with another prompt or
model, takes a lock, writes `results.json` atomically, treats a judge reply
without `other_medium_plus` as unusable, and re-judges a saved review instead
of rerunning it.

This baseline ran on the runner of commit 7dfbe2e, which was hardened
afterwards: settings isolation (`--setting-sources ""`), `git blame` and its
file-reading flags removed, denies built from the real paths instead of a
blanket `/home` and `/mnt`, the issue's comments limited to those written
before the head commit, one prompt per `--out` (the concurrency-lens runs
broke this), and a check that `gh` works before any review. A rerun with the
current runner is the next baseline.

Baseline (2 runs per case, plus one security-lens and one concurrency-lens
run on the ten cases with a class A or B finding; the full report is
`evals/review/baseline-2026-11-14.md`, total cost $55.27). The reviews read
the issue in 57 of 62 runs:

- Recall over 20 known findings: 83% single-run, 88% any-run. Known 79%
  single-run, missed 89%. Class A 79% (7 findings), B 83% (6), C 100% (1),
  E 63% (2), F 100% (3), I 75% (1).
- The one finding missed in every run, four of four including both lens runs:
  ganymede #817's High (a tailnet login loads `/lab` and gets the master
  token). The reviews found a different High in the same code.
- False alarms: 0.60 other Medium+ findings per run on the clean cases, and
  5 of 10 clean runs ended `Review: changes needed`. They may include real
  findings the posted review also missed; the judge doesn't check them.
- Two lenses vs one run, on 13 class A/B findings in 10 cases: one plain run
  81%, two plain runs 85%, a security-lens run plus a concurrency-lens run
  88%. The lens pair costs $17.32 against $9.07 for one run. Not justified
  by this sample: the pair beats one run by 7 points, under the 15 the rule
  asks for, and beats two plain runs by 3 points, which is half a finding (one "partly"
  became "caught").
- The prompt's Gaps entries name most of these PRs, so the baseline is
  optimistic for them. Its use is to catch a regression and to test a new
  entry against its own case, which should be missed before the edit and
  caught after.
- The reviews can't build or run tests here, which understates class E.
- Cases `ganymede-827` and `ganymede-827-copy-test` (Gaps entries 17 and 18)
  were added after the baseline ran and are not in its numbers.
- Gaps entry 17 was added to the prompt while the runs went, so the
  concurrency-lens runs used a prompt one entry longer (sha 0f3b111e against
  f4471519); each run records its sha.
