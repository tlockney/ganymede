## 2026-10-18, the site deploys from main again (#152)

Deploying only from release tags (#117) kept the docs in step with what
`install.sh` installs, but held every docs fix until a release or a manual
`pages` run. Thomas chose to deploy from `main` again and mark unreleased
features in the docs instead.

- `pages.yml` deploys on a push to `main` that touches `site/`, `docs/`,
  or the workflow. A manual run deploys `main` or the given ref.
  `release.yml` still dispatches it after publishing, without a tag now,
  so the site picks up the new version and changelog.
- Docs for a feature not in the latest release carry a GitHub-style
  `> [!NOTE]` alert starting `**Unreleased.**`. VitePress renders those
  alerts too, so one syntax works on both. The fixed words are what
  `release.sh` greps for; it prints the matches and lists them in the
  release pull request, and they are removed on the release branch.
- The rule lives in a new root `CLAUDE.md`, linked from development.md
  and releasing.md.
