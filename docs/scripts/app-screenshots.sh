#!/usr/bin/env bash
# Builds the Ganymede Mac and iOS apps from a ganymede-ui checkout and shoots
# them for the landing page: {mac,ipad,iphone}-{light,dark}.png.
#
#   app-screenshots.sh [flags] UI_CHECKOUT OUT_DIR
#     --skip-build        use the apps from the last build
#     --build-only        build, shoot nothing
#     --only mac|ipad|iphone   shoot one device
#     --mac-no-open       don't use `open` (for shells without Launch Services):
#                         start the app at the notebook's folder and wait for
#                         someone to click Orbits in its sidebar
#     --keep-error-cell   keep the fixture's deliberate TypeError cell
#     --mac-launch light|dark    only open the Mac app in that appearance
#     --mac-capture light|dark   only capture the Mac app already open, once it
#                         is the front window. These two split a Mac shot when
#                         the shell with Launch Services lacks Screen Recording.
#
# Env: WORK (build products, default ./work next to this script; no dot in its
# path, since the server treats dot-folders as missing), CONFIG
# (Debug), IPAD_UDID, IPHONE_UDID (default: largest iPad Pro, newest iPhone Pro).
#
# iOS has no scriptable way to open a notebook or rotate a device, so the
# script stops and asks: press Return when run in a terminal, otherwise
# `touch $WORK/go`. Needs ganymede and cdp-kernel installed (to execute the
# notebook). See docs/development.md, "App screenshots".
set -euo pipefail

skip_build=0 build_only=0 only=all mac_open=1 keep_error=0 mac_launch='' mac_capture=''
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) skip_build=1; shift ;;
    --build-only) build_only=1; shift ;;
    --only) only=$2; shift 2 ;;
    --mac-no-open) mac_open=0; shift ;;
    --keep-error-cell) keep_error=1; shift ;;
    --mac-launch) mac_launch=$2; skip_build=1; shift 2 ;;
    --mac-capture) mac_capture=$2; skip_build=1; shift 2 ;;
    -*) echo "unknown flag: $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
[ $# -eq 2 ] || { sed -n '2,26p' "$0" >&2; exit 2; }
ui=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
work=${WORK:-$here/work}
mkdir -p "$work"
derived=$work/DerivedData
config=${CONFIG:-Debug}
bundle=com.ganymede-server.Ganymede
xc_flags=(-skipPackagePluginValidation -onlyUsePackageVersionsFromResolvedFile
  CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual DEVELOPMENT_TEAM=)

build() {
  # The FFI revision CI builds this UI against, not whatever a checkout is on.
  local ref origin
  ref=$(sed -n 's/^ *GANYMEDE_REF: *//p' "$ui/.github/workflows/ci.yml" | head -1)
  origin=$(git -C "$ui" remote get-url origin | sed 's/ganymede-ui/ganymede/')
  if [ ! -d "$work/ganymede/.git" ]; then
    git clone --quiet "$origin" "$work/ganymede"
  fi
  git -C "$work/ganymede" fetch --quiet --tags origin
  git -C "$work/ganymede" checkout --quiet --detach "$ref"
  echo "== ganymede at $ref"

  # bootstrap.sh only installs when node_modules is missing, which goes stale
  # across pulls.
  (cd "$ui" && npm ci)
  GANYMEDE_DIR=$work/ganymede "$ui/apple/scripts/bootstrap.sh"

  echo "== Mac app"
  xcodebuild build -project "$ui/apple/Ganymede.xcodeproj" -scheme Ganymede \
    -configuration "$config" -destination platform=macOS \
    -derivedDataPath "$derived" "${xc_flags[@]}"
  echo "== iOS Simulator app"
  # arm64 only: the XCFramework's simulator slice has no x86_64, which the
  # generic destination would also build.
  xcodebuild build -project "$ui/apple/Ganymede.xcodeproj" -scheme Ganymede-iOS \
    -configuration "$config" -destination 'generic/platform=iOS Simulator' ARCHS=arm64 \
    -derivedDataPath "$derived" "${xc_flags[@]}"
}

# Executes a copy of the fixture so it has outputs, then writes one copy per
# platform: the Mac's is trusted, the iOS one names the kernel iOS has
# (otherwise its toolbar says the fixture's node kernel is "not installed").
prepare_notebooks() {
  local nb=$work/nb
  # The Mac sidebar shows the folder's name.
  rm -rf "$nb"; mkdir -p "$nb/mac/Notebooks" "$nb/ios"
  cp "$ui/e2e/fixtures/Orbits.ipynb" "$nb/Orbits.ipynb"
  ganymede execute --inplace --allow-errors "$nb/Orbits.ipynb"
  KEEP_ERROR=$keep_error python3 - "$nb" <<'PY'
import json, os, sys
root = sys.argv[1]
n = json.load(open(f"{root}/Orbits.ipynb"))
if os.environ["KEEP_ERROR"] != "1":
    n["cells"] = [c for c in n["cells"]
                  if not any(o["output_type"] == "error" for o in c.get("outputs", []))]
json.dump(n, open(f"{root}/mac/Notebooks/Orbits.ipynb", "w"), indent=1)
n["metadata"]["kernelspec"] = {
    "name": "cdp-browser", "display_name": "TypeScript (cdp-kernel)", "language": "typescript"}
json.dump(n, open(f"{root}/ios/Orbits.ipynb", "w"), indent=1)
PY
  ganymede trust "$nb/mac/Notebooks/Orbits.ipynb"
}

# Waits for a person (or a tool) to do something the script can't.
ask() {
  local go=$work/go
  rm -f "$go"
  echo ">> $1"
  if [ -t 0 ]; then
    read -r -p ">> press Return when done " _
  else
    echo ">> then: touch $go"
    local i
    for ((i = 0; i < 1800; i++)); do
      [ -e "$go" ] && { rm -f "$go"; return 0; }
      sleep 2
    done
    echo "timed out waiting for $go" >&2; return 1
  fi
}

pick_device() { # regex
  xcrun simctl list -j devices available | python3 -c '
import json, re, sys
devs = json.load(sys.stdin)["devices"]
found = [d for r, ds in devs.items() if "iOS" in r for d in ds if re.search(sys.argv[1], d["name"])]
found.sort(key=lambda d: [int(x) for x in re.findall(r"\d+", d["name"])])
print(found[-1]["udid"])' "$1"
}

shoot_ios() { # name udid instruction
  local name=$1 dev=$2 how=$3
  local app=$derived/Build/Products/$config-iphonesimulator/Ganymede.app
  xcrun simctl boot "$dev" 2>/dev/null || true
  xcrun simctl bootstatus "$dev" -b >/dev/null
  xcrun simctl status_bar "$dev" override --time 9:41 --batteryState charged \
    --batteryLevel 100 --cellularBars 4 --wifiBars 3
  xcrun simctl ui "$dev" appearance light
  xcrun simctl terminate "$dev" "$bundle" 2>/dev/null || true
  xcrun simctl install "$dev" "$app"
  # Answers the first-launch crash report question: it would cover the app.
  xcrun simctl spawn "$dev" defaults write "$bundle" crashReports -bool NO
  local docs
  docs=$(xcrun simctl get_app_container "$dev" "$bundle" data)/Documents
  mkdir -p "$docs"
  cp "$work/nb/ios/Orbits.ipynb" "$docs/Orbits.ipynb"
  xcrun simctl launch "$dev" "$bundle" >/dev/null
  sleep 5
  ask "$name ($dev): $how"
  sleep 5
  xcrun simctl io "$dev" screenshot "$out/$name-light.png"
  xcrun simctl ui "$dev" appearance dark
  sleep 4
  xcrun simctl io "$dev" screenshot "$out/$name-dark.png"
  xcrun simctl ui "$dev" appearance light
}

# Prints "<id> <width> <height> <title>" for a process's windows; with
# "screen", the main display's size in points.
windows_tool() {
  local tool=$work/windows
  [ -x "$tool" ] && { echo "$tool"; return; }
  cat > "$work/windows.swift" <<'SWIFT'
import CoreGraphics
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as! [[String: Any]]
if CommandLine.arguments[1] == "screen" {
  let b = CGDisplayBounds(CGMainDisplayID())
  print(Int(b.width), Int(b.height))
} else if CommandLine.arguments[1] == "canrecord" {
  print(CGPreflightScreenCaptureAccess())
} else if CommandLine.arguments[1] == "front" {
  // The list is ordered front to back.
  print(list.first { ($0[kCGWindowLayer as String] as? Int) == 0 }?[kCGWindowOwnerPID as String] ?? 0)
} else {
  let pid = Int(CommandLine.arguments[1])!
  for w in list where (w[kCGWindowOwnerPID as String] as? Int) == pid && (w[kCGWindowLayer as String] as? Int) == 0 {
    let b = w[kCGWindowBounds as String] as! [String: Any]
    print(w[kCGWindowNumber as String]!, b["Width"]!, b["Height"]!, w[kCGWindowName as String] ?? "-")
  }
}
SWIFT
  xcrun swiftc "$work/windows.swift" -o "$tool"
  echo "$tool"
}

stop_mac() {
  pkill -f "$mac_app/Contents/MacOS/Ganymede" 2>/dev/null || true
  sleep 2
}

# The app's appearance is set for this process only, not the system's.
# Preferences go to $work/home so other Ganymede builds on this Mac, which share
# the bundle id, keep their folder, recents and window frames.
launch_mac() { # light|dark
  local style=$1 tool size pid
  tool=$(windows_tool)
  size=$("$tool" screen)
  mkdir -p "$work/home"
  local args=(-ApplePersistenceIgnoreState YES -rootFolder "$work/nb/mac/Notebooks"
    "-NSWindow Frame workspace-AppWindow-1" "95 50 1280 820 0 0 $size ")
  if [ "$style" = light ]; then
    args+=(-NSRequiresAquaSystemAppearance YES)
  elif [ "$(defaults read -g AppleInterfaceStyle 2>/dev/null)" != Dark ]; then
    echo "warning: this Mac is in light mode; mac-dark.png needs the system in dark mode" >&2
  fi
  stop_mac
  if [ "$mac_open" = 1 ]; then
    open -n -a "$mac_app" --env "CFFIXED_USER_HOME=$work/home" "$work/nb/mac/Notebooks/Orbits.ipynb" --args "${args[@]}"
  else
    # Detached so the app outlives this script and the shell running it.
    pid=$(CFFIXED_USER_HOME=$work/home python3 -c '
import subprocess, sys
print(subprocess.Popen(sys.argv[1:], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, start_new_session=True).pid)' \
      "$mac_app/Contents/MacOS/Ganymede" "${args[@]}")
    echo ">> mac-$style: click Orbits in the Ganymede window's sidebar (pid $pid)"
  fi
}

capture_mac() { # light|dark
  local style=$1 tool pid w i
  tool=$(windows_tool)
  # Without Screen Recording, macOS hides window titles and refuses the capture.
  if [ "$("$tool" canrecord)" != true ]; then
    echo "This shell lacks Screen Recording, so it can't capture the window." >&2
    echo "Run --mac-capture $style from a shell that has it." >&2
    return 1
  fi
  # With --mac-no-open the window shows the folder until Orbits is clicked.
  local want=''
  [ "$mac_open" = 1 ] || want=Orbits
  for ((i = 0; i < 1800; i++)); do
    pid=$(pgrep -n -f "$mac_app/Contents/MacOS/Ganymede" || true)
    if [ -n "$pid" ]; then
      w=$("$tool" "$pid" | awk -v t="$want" '$2 > 400 && index($0, t) { print $1; exit }')
      [ -n "$w" ] && break
    fi
    sleep 2
  done
  [ -n "${w:-}" ] || { echo "timed out waiting for the Ganymede window" >&2; return 1; }
  # A window in the background has a greyed-out toolbar.
  echo ">> mac-$style: waiting up to 5 minutes for Ganymede to be the front window"
  for ((i = 0; i < 150; i++)); do
    [ "$("$tool" front)" = "$pid" ] && break
    sleep 2
  done
  [ "$("$tool" front)" = "$pid" ] || echo "warning: Ganymede is not in front; its toolbar will look inactive" >&2
  sleep 6
  "$tool" "$pid"
  screencapture -o -l "$w" "$out/mac-$style.png"
  echo "wrote $out/mac-$style.png"
}

shoot_mac() { # light|dark
  launch_mac "$1"
  capture_mac "$1"
}

[ "$skip_build" = 1 ] || build
[ "$build_only" = 0 ] || exit 0

mac_app=$derived/Build/Products/$config/Ganymede.app
if [ -n "$mac_capture" ]; then
  capture_mac "$mac_capture"
  exit 0
fi
prepare_notebooks
if [ -n "$mac_launch" ]; then
  launch_mac "$mac_launch"
  exit 0
fi

if [ "$only" = all ] || [ "$only" = ipad ]; then
  shoot_ios ipad "${IPAD_UDID:-$(pick_device '^iPad Pro 13')}" \
    "rotate to landscape (Device > Rotate Left), tap Orbits in the sidebar"
fi
if [ "$only" = all ] || [ "$only" = iphone ]; then
  shoot_ios iphone "${IPHONE_UDID:-$(pick_device '^iPhone \d+ Pro$')}" \
    "tap Orbits in the list"
fi
if [ "$only" = all ] || [ "$only" = mac ]; then
  shoot_mac light
  shoot_mac dark
fi
echo "done: $out"
