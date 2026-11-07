## 2026-11-07, install.sh checks the binary runs before installing it (#490)

`site/install.sh` moved the downloaded binary into place, then printed
`Installed $(ganymede --version)`. A binary that couldn't run (built for
another system, a missing loader or library) printed an empty version
and the script still exited 0, having replaced a working install.

The script now runs `--version` on the new copy before the rename, and
on failure removes it, keeps the old binary, and exits with the error
output. `scripts/test-install.sh` packages a 9.9.8 stub that exits 126
and checks both the failure and that the 9.9.9 install is left alone.
