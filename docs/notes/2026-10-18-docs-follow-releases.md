## 2026-10-18, the site deploys from releases, not main (#117)

`ganymede lab` printed its usage on v0.1.0: that tag predates #33, which
added the bare command, but the docs site deployed from main on every
merge, so the workstation page described it before any release had it.
The CI `workstation` job builds the PR's own binary, so it could not see
the gap.

- `pages.yml` no longer deploys on push. `release.yml` dispatches it with
  the new tag after `gh release create`, and the build checks out that
  tag, so the docs and `install.sh` match what `install.sh` installs. A
  manual run with no `ref` deploys the latest release; with a `ref` it
  publishes a docs fix between releases.
- The dispatch runs on `main` (`--ref main`, the workflow file's ref) and
  passes the tag as an input. Running the workflow on the tag itself
  would make the `github-pages` environment, which by default only
  allows the default branch, refuse the deploy.
- A workflow started with `GITHUB_TOKEN` triggers nothing, except
  `workflow_dispatch`, which is why tag-release.yml and release.yml chain
  through `gh workflow run`.
