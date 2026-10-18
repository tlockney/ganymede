## 2026-10-18, `ganymede info` shows Ganymede's own paths (#57)

`info` printed Jupyter's runtime, config and data dirs and nothing of
Ganymede's own: not the data dir (`GANYMEDE_DATA_DIR`), the JupyterLab
installs or extensions under it, the notary dir, the console history, or
the `--kernelspec-root` extras. A bug report could not say where things
were, and nothing gave the commit or target a build came from.

- `info` and `info --json` add `data_dir`, `lab_dir`, `lab_versions`,
  `labextensions_dir`, `extensions` (name, version, dir), `kernelspec_roots`
  (extras first, as searched), `notary_dir`, `console_history`, and
  `build` (commit, target, profile). Existing keys are unchanged.
- The build details are in `info`, not `--version`: the install scripts
  and the release workflow parse `ganymede --version` as one line,
  `ganymede X.Y.Z`. A `build.rs` reads them; outside a git checkout the
  commit can be passed in `GANYMEDE_BUILD_COMMIT`.
- The paths come from the same functions the commands use
  (`ganymede_server::lab::data_dir`, `console::history_path`), so `info`
  cannot drift from what is actually read.
