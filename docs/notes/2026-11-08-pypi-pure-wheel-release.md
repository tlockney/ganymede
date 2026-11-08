## 2026-11-08, lab install picks a release with a pure wheel (#537)

`newest_matching` chose the newest matching release with any unyanked
file. When that release had only an sdist or platform wheels,
`install_wheel` found no `py3-none-any` wheel and the install failed, even
though an older release had one.

The pure-wheel rule applies only to packages that get installed. The
install loop also resolves every dependency it walks (`tornado`, `pyzmq`,
`numpy`) to read its classifiers and requirements, and those have no pure
wheel. So `newest_matching` and `resolve` take a `pure_only` flag. The walk
resolves without it, and a package that is about to be installed is
resolved again with it when its release has no pure wheel. A lone `==` pin
is fetched directly as before, so a pinned release with no pure wheel
still fails in `install_wheel`.
