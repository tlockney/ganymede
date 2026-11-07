## 2026-11-07, install-script's macOS checks move into workstation (#459)

A ganymede PR took four macOS runners, and macOS runners are what PRs
across the four repos queue for: in run 608, `install-script
(macos-latest)` waited 6.6 min for 0.2 min of work. Its three checks
(`test-install.sh` under sh and bash, then with `TEST_KERNEL=1`) now run
at the end of `workstation (macos-latest)`, even when the workstation
test fails, and install-script runs on Linux only. Its matrix keeps the
one cell so the required check `install-script (ubuntu-latest)` keeps
its name; the macOS cell was never a required check.
