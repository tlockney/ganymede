## 2026-11-07, pin the default lab extensions (#433)

`ganymede lab install` pinned JupyterLab by version and hash but took the
newest `anywidget` and `jupyterlab_widgets`, and whatever their
dependencies allowed. PyPI's digest only shows a wheel is what PyPI
serves, so a compromised release would reach every new install as
JavaScript on the server's origin.

- `extension::DEFAULTS` is now a list of `Pin`s (name, version, wheel
  SHA-256). The defaults install as `name==version` with dependencies not
  followed, since the pinned list is complete; `install_wheel` refuses a
  wheel whose hash differs from its pin.
- `--latest-extensions` restores the old behaviour.
- `scripts/check-pins.sh`, run by a weekly `pins` job in ci.yml, fails
  when PyPI has a newer JupyterLab or default extension than the pins.
  Dependabot can't watch constants in Rust source. Its first run will flag
  JupyterLab 4.6.6 against the 4.6.4 pin.
