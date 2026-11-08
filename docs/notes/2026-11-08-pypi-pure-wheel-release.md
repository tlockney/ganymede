## 2026-11-08, lab install picks a release with a pure wheel (#537)

`newest_matching` chose the newest matching release with any unyanked
file, and `pick_wheel` then found no pure wheel in it when the release had
only an sdist or platform wheels, so the install failed even though an
older release had one. The resolver now counts only releases where
`pick_wheel` finds a file, so it falls back to the newest release it can
install. A pinned version with no pure wheel resolves to nothing, as a
yanked one does.
