## 2026-10-18, CLI gaps: completions, `lab clean`, extension toggles (#71)

The first half of the review's CLI list: commands a Python-free user had
no way to run, and flags with no help text.

- `ganymede completions <shell>` prints a completion script (via
  `clap_complete`) for bash, zsh, fish, PowerShell and elvish.
- `attach` is listed and documented. It was hidden, though it is
  `run --existing` with a 60-second default timeout. `run
  --interrupt-after-ms` is listed too.
- `lab clean` removes the `.download-*` and staging directories that
  interrupted installs leave. `lab uninstall` removes every JupyterLab
  version and extension `lab install` added.
- `lab extension disable/enable` write `disabledExtensions` in
  `labconfig/page_config.json` under the Jupyter config dir, in
  JupyterLab 4's `{"name": true}` shape, as `jupyter labextension disable`
  does. The server now reads that file on each page load (both the map
  and the older list shape); it used to always send `[]`.
- `lab list --json` and `lab extension list --json`.
- Help text for the positionals of `kernelspec install/remove`, `lab
  remove`, `lab extension remove`, `execute`, `nb clear` and `trust`;
  for `kernel -k`; and for `serve`'s `--ip`, `--root-dir`,
  `--kernelspec-root`, `--default-kernel` and `--cull-idle-secs`.
