## 2026-11-07, Unreleased notes for app changes (#502)

- The docs and landing page describe the apps, which ship from
  ganymede-ui. An "Unreleased." note there was removed at the next
  Ganymede release, whether or not ganymede-ui had shipped the change.
  The macOS 26 / Apple silicon floor (ganymede-ui#356) was the case in
  point: the landing page has to keep "macOS 13 or later, Apple silicon
  or Intel" until ganymede-ui releases, then change.
- App changes now take `**Unreleased in ganymede-ui.**`. Ganymede's
  release grep (`Unreleased\.`) skips it; `check-unreleased` accepts it.
  On the landing page it sits in an HTML comment beside text that is
  right until then, saying what to change.
- ganymede-ui's `scripts/release.sh` reads these notes from Ganymede's
  `main` and opens an issue here listing them (ganymede-ui#397).
