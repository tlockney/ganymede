## 2026-10-19, ganymede-ui's docs on this site (#175)

ganymede-ui's docs live in its own repository, next to the code they
describe, and this site renders them under `/docs/ganymede-ui/`.
`npm run fetch-ui` (`docs/scripts/fetch-ganymede-ui.sh`) copies its
`docs/*.md`, all but its dev log, and its `docs/images/` from `main` with a sparse, shallow clone
into `docs/ganymede-ui/`, which is ignored by git. `pages.yml` runs it
before the build; ganymede-ui's `docs` workflow sends a
`repository_dispatch` (`ganymede-ui-docs`) when those files change, which
rebuilds and deploys. A submodule would have needed a commit here for
every docs change there, and a separate site would have split the nav and
search.

`config.mts` gives those pages their own sidebar group, titled from each
page's first heading, with `architecture.md` first. Their relative links
resolve against ganymede-ui's `docs/`: links to another copied page stay
on the site, links to anything else (`../src/lib/backends.ts`) go to
ganymede-ui on GitHub. "Edit this page" points at ganymede-ui too. The
`editLink` pattern is serialized into the client bundle, so it can't use
the module's `repo` constant; it threw `repo is not defined` at render.

`/docs/ui` was already the "User interfaces" page, hence the longer path.
A link from these docs to a ganymede-ui page that isn't there (no copy
locally, or a rename over there) warns instead of failing the build, so a
change in that repository can't break this site's deploy.
