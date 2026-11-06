## 2026-11-06, apps on the site (#401)

- The landing page has an Apps section between "Serve, then connect" and
  "Get started": the Mac window full width, then iPad (landscape) and
  iPhone side by side at one height (each figure's `flex-grow` is its
  image's aspect ratio), light and dark copies like the hero.
- The Mac download button starts as a link to ganymede-ui's latest
  release page, and a small script swaps in the `Ganymede-macOS-X.Y.Z.zip`
  asset from the GitHub API. `releases/latest/download/` needs a fixed
  name, and the only fixed-name zip on v0.4.0, `Ganymede-macOS.zip`, is
  CI's ad-hoc signed build that `release.yml` publishes by mistake
  (ganymede-ui#334).
- iPad and iPhone are described as internal TestFlight only, with
  build-from-source as the way in; App Review (2.5.2) is the blocker for
  external testing. docs/app.md, known-gaps.md and roadmap.md no longer
  say the Mac app is an unreleased CI artifact.
- Shots were taken on a Mac from ganymede-ui 98e654f (Debug, ganymede-ffi
  v0.6.0) with `docs/scripts/app-screenshots.sh`. iOS used `xcrun simctl`
  (status bar override, appearance), with the iPad rotation and the tap on
  the notebook done by hand. The Mac shot is `screencapture -o -l` of a
  1280×820 window; light mode uses `-NSRequiresAquaSystemAppearance`, dark
  follows the system.
- Two traps: the server treats any path through a dot-folder as hidden,
  so a work folder named `.work` gave "no such file or directory" for a
  notebook that existed (ganymede-ui#337 for the Mac app's message); and
  without Screen Recording, `CGWindowListCopyWindowInfo` returns no window
  titles, so waiting on a title hangs silently. The script checks
  `CGPreflightScreenCaptureAccess` and can split launch and capture across
  two shells.
- Follow-up worth doing: a launch argument in the app to open a notebook,
  which would make the iOS shots unattended, or XCUITest screenshots on CI.
