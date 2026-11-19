## 2026-11-19, Install page: the CLI from the Mac app (ganymede-ui#686)

ganymede-ui#689 bundles the `ganymede` CLI in the Mac app and adds
Settings, General, Command Line Tool, which links it to
`~/.local/bin/ganymede`. `docs/install.md` gains "From the Mac app" under
"Other ways to install", with an `**Unreleased in ganymede-ui.**` note
that ganymede-ui's release script lists until its release ships. The
app bundles only a CLI from a ganymede release after 0.13.0, the first
with `ganymede update`'s app-bundle refusal (#883).
