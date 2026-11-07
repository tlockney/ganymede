## 2026-11-07, Windows stays best effort (#60)

#60 asked whether to invest in Windows CI or stop advertising Windows.
Thomas chose a third way: keep building and releasing the Windows
archive, since the server is meant to run anywhere, but call it untested
and best effort, with no plans for Windows CI. The README now says so
next to the platform list, and Install and Known gaps no longer point at
#60 as open work. The cross-compiled clippy check of the kernel host's
Windows code stays in CI.
