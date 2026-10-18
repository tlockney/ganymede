# Contributing

Work is tracked in [GitHub issues](https://github.com/tlockney/ganymede/issues).
Each change is a pull request that links the issue it resolves
(`Closes #N`), passes CI, and adds a short dev-log entry under
`docs/notes/`.

[Development](docs/development.md) has the build and test commands, the
project's rules (the host knows no language; the REST and websocket API
matches `jupyter_server`), the CI setup, and how to work on the docs.
[Releasing](docs/development.md#releases) covers cutting a release.

Report security problems privately, as [SECURITY.md](SECURITY.md) says,
not in a public issue.
