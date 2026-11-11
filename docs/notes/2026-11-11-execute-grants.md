## 2026-11-11, host API under `ganymede execute` (#647)

`ganymede execute` now answers its kernels' host calls with no server
running. It builds the server's state without a listener
(`serve::LocalHost`), rooted at the current directory, and answers with
the same `HostApi` the server uses, so grant checks, run nesting and run
width are one implementation. The engine's kernel isn't one the server
started, so `HostApi::pin` names its notebook; `unpin` forgets it when
the notebook's run ends.

Grants are `[host]` from the config file with `--grant read=PATH` or
`run=PATH` in place of each key named. There is no file id database, so
no notebook's own grant applies. `self.*` is off: `execute` writes the
notebook itself at the end, and an edit through a room would race that
write. If the local host can't start (no subprocess provisioner), the run
goes on without a host comm and says so on stderr.

The e2e test runs three `execute`s of a cdp-node notebook that reads
itself, a granted and an ungranted notebook, and runs another, under the
defaults, under `--grant`, and under a config file.
