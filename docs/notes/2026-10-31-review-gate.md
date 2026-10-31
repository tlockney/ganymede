## 2026-10-31, review gate before merging (#254)

Until now a PR's only reviewer was the thread that wrote it, and `main` had no branch protection, so a PR could merge with CI red. An audit of the four repos (#253) found broad CI but no independent review: #248 merged with no review, and six PRs merged within twenty minutes on 2026-10-31.

Required approvals don't fit: thread PRs are authored as `tlockney`, and GitHub won't let an author approve their own PR. So the gate is a label and a check. A separate review session adds `review: pass` when a PR passes; the `review-gate` workflow passes only with that label, and a push removes it, so every new commit is reviewed again. A token from a fork can't edit labels, so that step is allowed to fail and the reviewer drops the label instead. Label changes made with `GITHUB_TOKEN` start no workflow runs, so dropping the label doesn't re-trigger the gate.

The PR template's Verification checklist gives the reviewer something to check the diff against, and the `spike` label separates exploratory PRs (like #251) from production ones without lowering the bar for dependencies or advisory ignores. Rulesets that make `review-gate` and CI required are set in repo settings.
