## 2026-10-18, frontend/ moves to ganymede-ui (#134)

`frontend/` was TypeScript for a UI of Ganymede's own, and nothing in this
repository used it. That UI now exists as
[tlockney/ganymede-ui](https://github.com/tlockney/ganymede-ui), which took
the package with its history (`git subtree split --prefix=frontend`) as
`packages/client`. The package went with it, along with its CI job and
Dependabot entry. The core keeps no TypeScript; the REST and websocket API
is the only contract with any UI.
