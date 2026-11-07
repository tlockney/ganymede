## 2026-11-07, The kernel venv on uv's managed Python (#452)

`kernels.md` recommends uv for the global Python kernel. Plain `uv venv`
picks the first Python it finds, often Homebrew's, so a `brew upgrade`
could still pull the interpreter out from under the kernel, the failure
the global setup was meant to avoid. `uv venv --managed-python` always
uses a Python uv installs itself.

Checked with uv 0.11.32: the environment's `pyvenv.cfg` points at uv's
per-minor directory (`cpython-3.13-...`), and after `uv python upgrade
3.13` the same environment reported 3.13.14 instead of 3.13.1 without
being made again. A new minor version is a separate install, so the
environment stays put until it is remade.
