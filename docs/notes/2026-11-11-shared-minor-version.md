## 2026-11-11, one minor version with ganymede-ui (#717)

Since the release speed-up, Ganymede releases before ganymede-ui in a
round, so Ganymede reached 0.9.0 while the apps were at 0.8.0, and the
docs and site showed both. The two now share their minor version:
ganymede-ui's `release.sh minor` takes Ganymede's latest X.Y.0
(tlockney/ganymede-ui#511), and patches stay per repository. The next
round takes the apps from 0.8.0 to 0.10.0. The release order and pins
don't change.

reference.md named `ganymede ui install`'s default as `0.7.0`, a number
`release-pins.sh` moves in ui.rs but not in the docs, so it would have
gone stale each round. It now points at `--help`, which prints the pin.
