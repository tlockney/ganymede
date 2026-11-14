## 2026-11-14, Review replay eval (#822)

`evals/review/` replays the pre-push review prompt on 21 cases and scores
what it catches (part of #821). A case is a diff a reviewer saw (repo, base
sha, head sha, issue) with the known Medium or High finding and its class A
to I from the review-patterns analysis. Ten cases are first-round diffs of
posted `changes needed` reviews, six are diffs a passing review missed and a
later run found, five are clean diffs that passed with no follow-up. The
runner gives `review-prompt.md` word for word plus the workflow's trailer
(REPO, BRANCH, ISSUE, DIFF) to `claude -p` on Opus with read-only tools, in a
scratch clone whose `origin/main` is the case's base, so nothing after the
change is reachable. A Sonnet judge scores each review caught, partly or
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

Baseline (2 runs per case, plus one security-lens and one concurrency-lens
run on the ten cases with a class A or B finding; the full report is
`evals/review/baseline-2026-11-14.md`, total cost $60.71):

- Recall over 20 known findings: 85% single-run, 88% any-run. Known 85%,
  missed 86%. Class A 86% (7), B 83% (6), C 100% (1), E 75% (2), F 100% (3),
  I 50% (1).
- The one finding missed in every run, four of four runs, including both lens runs:
  ganymede #817's High (a tailnet login loads `/lab` and gets the master
  token). The reviews found a different High in the same code.
- False alarms: 0.70 other Medium+ findings per run on the clean cases, and
  5 of 10 clean runs ended `Review: changes needed`. They may include real
  findings the posted review also missed; the judge doesn't check them.
- Two lenses vs one run, on 13 class A/B findings in 10 cases: one plain run
  85%, two plain runs 85%, a security-lens run plus a concurrency-lens run
  88%. The lens pair costs $18.79 against $10.61 for one run. Not justified
  by this sample: the pair beats one run by 3 points, under the 15 the rule
  asks for, and a second plain run adds nothing here either.
- The prompt's Gaps entries name most of these PRs, so the baseline is
  optimistic for them. Its use is to catch a regression and to test a new
  entry against its own case, which should be missed before the edit and
  caught after.
- The reviews can't build or run tests here, which understates class E.
