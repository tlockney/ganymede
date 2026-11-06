# Ganymede app screenshots: notes

For the ganymede-server.com landing page (tlockney/ganymede#401). Re-shoot with:

```sh
./app-screenshots.sh ~/src/personal/ganymede-ui .               # build, then shoot
./app-screenshots.sh --skip-build ~/src/personal/ganymede-ui .  # shoot the last build
```

## Environment

- Xcode 27.2 (27B52), iOS 27.2 simulators, Node 26.9 (the README asks for 24; it built)
- ganymede-ui `98e654f` (`main`), Debug build
- ganymede FFI: `v0.6.0`, the `GANYMEDE_REF` in ganymede-ui's `ci.yml`, from a clone
  in `work/ganymede`. `~/src/personal/ganymede` was not touched.
- iPad Pro 13-inch (M5) `F927BD06-8479-49DC-AF10-BBAE061A2484`, landscape, 2752x2064
- iPhone 18 Pro `239638F9-23F9-49FF-909C-2C9CFA2CDD3E`, portrait
- Mac window 1280x820 points (2560x1640 PNG on this display)

## Build commands

The script runs these; `work` holds the products.

```sh
npm ci
GANYMEDE_DIR=work/ganymede apple/scripts/bootstrap.sh
xcodebuild build -project apple/Ganymede.xcodeproj -scheme Ganymede -configuration Debug \
  -destination platform=macOS -derivedDataPath work/DerivedData \
  -skipPackagePluginValidation -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=
xcodebuild build -project apple/Ganymede.xcodeproj -scheme Ganymede-iOS -configuration Debug \
  -destination 'generic/platform=iOS Simulator' ARCHS=arm64 -derivedDataPath work/DerivedData \
  -skipPackagePluginValidation -onlyUsePackageVersionsFromResolvedFile \
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=
```

`ARCHS=arm64` is needed: the generic destination also builds x86_64, and the
XCFramework's simulator slice is arm64 only, so the link fails without it.

## What is scripted and what is not

Scripted: building, executing and trusting the notebook, booting simulators, the
status bar, appearance, installing, copying the notebook, launching, capture.

Needs a person or a tool:

- **iPad rotation.** `simctl` can't rotate. Device > Rotate Left in Simulator, or the
  Claude Code simulator tool. It stays rotated while the simulator stays booted.
- **Opening the notebook on iOS.** The app has no URL scheme or launch argument for
  it, only a tap (`page.run("open")`). The script waits for Return in a terminal,
  otherwise for `touch work/go`.
- **Opening the notebook on the Mac, only with `--mac-no-open`.** Without the flag the
  script uses `open -n -a`, which opened it correctly from Thomas's terminal.

A launch argument or URL scheme for opening a notebook would make all of this
unattended, but that is an app change and I did not make it.

## Choices that affect the pictures

The shots use copies in `work/nb`; `e2e/fixtures/Orbits.ipynb` is unchanged.

- **The fixture's last cell is removed.** It is a deliberate `TypeError`, a red
  traceback at the bottom of the page. `--keep-error-cell` keeps it.
- **The iOS copy names `cdp-browser`.** The fixture names `cdp-node`, which iOS can't
  run, so the toolbar showed a red "(not installed)".
- **The crash reports question is answered "no"** in each simulator's defaults, or its
  dialog covers the first launch.
- **The Mac app keeps its preferences in `work/home`** (`CFFIXED_USER_HOME`), since
  other Ganymede builds share its bundle id. Its window frame and appearance are launch
  arguments. Nothing in the system's settings changes.
- **Mac light mode** comes from `-NSRequiresAquaSystemAppearance YES`.
  `-AppleInterfaceStyle Light` had no effect. **Mac dark mode** just follows the system,
  so `mac-dark.png` needs the Mac in dark mode; the script warns if it isn't.
- `ganymede trust` records the Mac copy's signature in the local trust database.
- The status bar's date is not overridden (only 9:41), so it shows the day of the shoot.

## Keep work out of dot-folders

The Mac server serves `/`, and it treats any path with a dot-folder in it as hidden, reporting
"no such file or directory" even though the file exists. The working folder was first `.work`,
and the app showed "Couldn't open Orbits.ipynb". The same applies to anyone opening a notebook
under a dot-folder from the Finder. Reproduced with `ganymede serve --root-dir /`: 200 for the
plain path, 404 for a dotted one. The missing leading `/` in the message is normal for API paths.

## Not verified

- **One unattended Mac run.** `--only mac` from one shell needs that shell to have both
  Launch Services and Screen Recording. It hung under Rex waiting on the window title, and
  I replaced that wait, but no single shell here has run the current version end to end.
  The cause, Rex lacking Screen Recording, is my inference from titles being readable from
  my shell and not from the script; I did not check Rex's permission. The script now checks
  and says so.
- **Shooting from a Mac in light mode.** This Mac is in dark mode, so `mac-dark.png` from
  a light-mode Mac is untested; the script warns and would produce a light picture.

## How the Mac pair was shot

Launching and capturing ran in different shells: Thomas's terminal has Launch Services, and
mine (the Claude desktop app's, sandbox off) has Screen Recording.

```sh
./app-screenshots.sh --mac-launch light ~/src/personal/ganymede-ui .   # terminal with `open`
./app-screenshots.sh --mac-capture light ~/src/personal/ganymede-ui .  # shell with Screen Recording
```

Then the same with `dark`. `--mac-capture` waits for Ganymede to be the front window,
since a background window has a greyed-out toolbar. Thomas's first light launch came from
the older `--only mac` command, which uses the same `open` line.

## Status of this shoot

All six PNGs are done. The iOS taps and the iPad rotation came from the Claude Code
simulator tool, which needs its per-device access granted (opening its panel for the
device did it for the iPhone). No privacy settings were changed.

Visible in the shots:

- iPhone: long code lines are clipped at the cell's edge and the `[2]` output wraps its
  last digit onto a second line. That is how the app renders at this width.
- Mac: the first cell is selected, so its action icons show and the toolbar's cell type
  reads Markdown. The kernel line reads `cdp-kernel, node`, the Mac's process kernel.
- iPad: the window-resize handle shows in the bottom-right corner, and the status bar
  shows the shoot's date (only the time is overridden).
