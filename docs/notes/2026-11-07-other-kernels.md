## 2026-11-07, An "Other kernels" page (#446)

The docs said ipykernel works "once its kernelspec is installed" and
stopped there. `kernels.md` now walks through it: install ipykernel in an
environment, register it with `ipykernel install --user --name`, and
check it with `ganymede run`. Every command was run against ipykernel 7.4
and a local build.

The venv case gets its own section because it differs from Jupyter.
`pip install ipykernel` drops a `python3` spec in `<venv>/share/jupyter`
whose `argv` is plain `python`; `jupyter_client` finds it through
`sys.prefix` and swaps in its own interpreter, Ganymede does neither. So
the page recommends `--user --name`, which writes an absolute path, and
shows `--kernelspec-root` plus an active environment as the alternative.

The page also has the discovery order as a diagram and a short table of
other kernels (Deno, IRkernel, IJulia, evcxr, gophernotes, xeus-cpp,
Almond) with links, marked as untested. IRkernel's `installspec()` shells
out to `jupyter`, so its row shows `ganymede kernelspec install` instead.
