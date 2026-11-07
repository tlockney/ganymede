## 2026-11-07, One global Python kernel (#448)

`kernels.md` first registered ipykernel from whatever environment the
reader had at hand. A kernelspec stores that environment's Python by
path, so moving or deleting the environment, or upgrading the Python it
was made from, leaves a kernel that is listed but won't start. People
who don't usually manage Python environments hit that without knowing
why.

The page now leads with one environment kept only for the kernel, at
`~/.local/share/venvs/python3`, registered as `python3`, the name most
notebooks record. It shows the error a moved environment gives and how
to recover, advises against the system and Homebrew Pythons (PEP 668),
and adds an all-users variant under `/usr/local`. Per-project kernels
follow as a second option. All of it was run against ipykernel 7.4 and
a local build, including a user `python3` hiding the `/usr/local` one.
