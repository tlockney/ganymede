## 2026-11-01, Agents docs page (#241)

- New [Agents](../agents.md) page: nb-cli against `ganymede serve`,
  Claude Code setup (nb-cli's `notebook-cli` skill and a `CLAUDE.md`
  rule against Read/Write on `.ipynb`), journaling conventions,
  recording steps that ran outside the kernel, checking with `nb search
  --with-errors` and `ganymede execute`, and what doesn't work yet.
- Flags were checked against nb-cli's source rather than its README:
  `cell update` takes `--type`, `--source` unescapes `\n`, and `nb
  execute`'s default timeout is 30 seconds.
- Only the `jpserver-<pid>.json` part depends on unreleased behaviour, so
  it is its own subsection with the Unreleased note; nb-cli itself works
  against v0.4.0.
- Review fixes: nb-cli 0.0.10's `nb execute` exits 1 and stops at the
  first failing cell (the page first said 0); JupyterLab without
  collaboration doesn't notice disk changes until you save, and
  Overwrite then drops the agent's cells; `--source` unescapes `\n`, so
  code goes in on stdin.
- `main` now serves collaboration rooms, which nb-cli probes for and
  would edit through. The validation predates them, so the page
  describes v0.4.0's contents-API path and #278 re-runs it on `main`.
- Second review: nb-cli reads `.jupyter/cli.json` from the current
  directory only and falls back to local mode without it, and `nb
  create` checks for an existing notebook locally before writing to the
  server, so the page says to run `nb` from the server's root dir. The
  rooms-on-`main` sentence is an Unreleased note so the release step
  lists it.
