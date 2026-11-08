## 2026-11-08, Windows: no colons in API paths (#545)

`ApiPath::parse` refused backslashes, a leading slash and `..`, but on
Windows a component such as `C:x` is drive-relative: joined onto the
contents root it replaces it, so the request reached a path on drive C:
outside the root. `a:b` also opened an NTFS stream of `a`. On Windows a
path with a colon is now `OutsideRoot`, like a backslash. Other platforms
keep allowing colons in names. `parse_for` takes the platform as an
argument so the rule is tested on every CI runner, since Windows has none.
