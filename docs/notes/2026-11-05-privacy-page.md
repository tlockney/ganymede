## 2026-11-05, privacy page for crash reporting (#385)

- `docs/privacy.md` states the crash reporting rules from #384 before any
  reporting code lands, so the opt-in UIs and help text can link one
  page. It sits under Guides beside Security, and the README links it.
- It fixes the opt-in names #386 implements: `crash_reporting = true` in
  `ganymede.toml`, and `GANYMEDE_CRASH_REPORTING=1`/`0`, which overrides
  the file. Only binaries built with the `crash-reporting` cargo feature
  (release builds) can send.
- The page's Unreleased note also says no build sends anything yet; #386
  drops that sentence when the server and CLI can report.
