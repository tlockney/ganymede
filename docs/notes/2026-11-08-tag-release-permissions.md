## 2026-11-08, tag-release.yml job permissions (#538)

`tag-release.yml` granted `contents: write` and `actions: write` to the
whole workflow, against the rule in docs/development.md that a workflow
starts from `contents: read` and a job asks for more itself. The `tag`
job, the only one, now holds both. It runs only when a release pull
request merges, so the first release after this change exercises it.
