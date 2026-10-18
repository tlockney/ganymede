## 2026-10-18, CLI behaviour: exit codes, stdin, `--json` gaps (#71)

The second half of the review's CLI list: places where the commands did
something a script could not see or rely on.

- `ganymede run -k fake </dev/null` ran an empty cell and exited 0. Empty
  or blank stdin is now a usage error, exit 2, like clap's own.
- `run --json` printed nothing for a cell that timed out or whose kernel
  died, only an error on stderr, so "stopped here" looked like "ended". It
  now prints a line with `"status": "incomplete"` and the error.
- `execute` stopped the whole run on an unreadable notebook or a kernel
  that would not start, but carried on past a failing one. Each kind is
  now reported and skipped, and the exit status is 1 if any failed.
- Exit statuses are documented in `--help` and getting-started.md: 0, 1
  for any failure (as `jupyter execute`), 2 for usage, 128+N for signals.
- `kernelspec remove` asks first when stdin is a terminal, as `jupyter
  kernelspec remove` does; `-f`/`-y` skips the question. Scripts, with no
  terminal, are not asked.
- `jupyter execute`'s flag names (`--kernel_name`, `--startup_timeout`,
  `--skip-cells-with-tag`, `--allow-error-names`) are accepted as aliases.
- Console: password prompts turn off echo (with `stty`, on a terminal),
  and history is saved after every cell, not only on `:quit` or Ctrl-D.

Not done: `kernel list`, `server list` and `lab stop`, which need a
design; quieting the global flags on commands they don't affect; and
moving the console history out of the Jupyter data dir.
