# CLAUDE.md

Guidance for Claude working in this repository. [CONTRIBUTING.md](CONTRIBUTING.md)
and [Development](docs/development.md) have the build, test, and review
rules; this file adds what they don't.

## Documenting unreleased features

The site deploys from `main` on every merge (`.github/workflows/pages.yml`),
but `install.sh` installs the latest release. So when a change documents
a command, flag, or behavior that the latest release doesn't have, add
this note next to it, in the docs page, README, or landing page:

```md
> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.
```

It renders as a note on GitHub and on the site. Keep the words
`**Unreleased.**` as they are: the release looks for them. Put one note
per section the feature adds, not one per paragraph. A change to a
feature that is already released needs no note, and `site/install.sh`
goes live on merge, so it must keep working with the latest release.

## Cutting a release

Releases follow [Releasing](docs/releasing.md). Before the release pull
request merges, review every "Unreleased" note:

```sh
grep -rn 'Unreleased\.' README.md docs/*.md site/index.html
```

`scripts/release.sh --execute` prints them and lists them in the pull
request. The release ships everything on `main` when the branch was
cut, so on the release branch remove each of those notes, and reword the
text around a note if it read as unreleased. A note added to `main` after
the branch was cut stays.
