## 2026-11-09, Privacy page: the Mac app's update check (#599)

ganymede-ui's Mac app now checks GitHub daily for a new release, on by
default (tlockney/ganymede-ui#411). The privacy page said crash reporting
was the only exception to "no telemetry", so it now describes the check:
one GET of the release feed, nothing about the user sent, off in
Settings. It carries the `Unreleased in ganymede-ui` note until that
release.
