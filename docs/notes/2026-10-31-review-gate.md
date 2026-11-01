## 2026-10-31, independent review of every PR (#254)

Until now a PR's only reviewer was the thread that wrote it. An audit of the four repos (#253) found broad CI but no independent review: #248 merged with no review, and six PRs merged within twenty minutes on 2026-10-31.

The fix is eval-style vetting, not a human gate: a scheduled Claude session that didn't write the PR reviews it once CI is green, checks it against its issue and the PR template's Verification checklist, and posts a verdict. It is advisory and never waits on a person; the point is to be able to trust a PR without reading its code. A label-plus-required-check gate was tried first and dropped: GitHub counts thread PRs as authored by `tlockney`, so the gate had to fake an approval, and requiring review is against the project's approach of agents vetting agents.

The stronger evals are behavioural: tests against real clients (#256, #257, #258) and coverage reports (#255). The `spike` label separates exploratory PRs (like #251) from production ones without lowering the bar for dependencies or advisory ignores.
