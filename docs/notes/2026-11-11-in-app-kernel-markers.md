## 2026-11-11, docs markers for the in-app kernel in the apps (#266)

ganymede-ui#470 registers the apps' TypeScript kernel with their server,
so `docs/app.md` and `docs/ipados.md` describe the apps as they are in
ganymede-ui's latest release but not its `main`. They keep their text
until that release, with `**Unreleased in ganymede-ui.**` notes saying
what `main` does instead, which ganymede-ui's release script lists. In
Markdown the notes must be visible `> [!NOTE]` blocks
(`docs/scripts/check-unreleased.mjs`); HTML-comment markers are only for
`site/index.html`. The note in
`docs/in-app-kernel.md` now says the apps' part is built.
