## 2026-10-18, docs checked against what shipped (#66)

The review found docs promising what the code didn't do, and since then
several of those promises have shipped. A pass over each claim:

- [Roadmap](../roadmap.md): cdp-kernel conformance, kernel lifetime
  (reconnect replay, death reporting, relaunch, shutdown steps),
  `ganymede lab` and its installs, `install.sh`, and the workstation job
  moved to Done. Open gained auto-restart, the remaining conformance
  gaps, nbformat 3, and musl.
- [Protocol](../protocol.md) stays a requirements list, but each item
  Ganymede doesn't meet now says so: no `sys.prefix` kernelspec root (on
  purpose), heartbeat only for attached kernels, no autorestart, no
  Windows job object, no terminals, assets from disk, no v3.
- The language rule said a grep for `python` in the host crates "finds
  nothing". It finds nbformat's float layout, the trust signature, PyPI
  host names, and comments. The rule is about behaviour, and is reworded
  to say that: no code depends on a kernel's language.
- `metadata.execution.timeout` on a cell was documented "as in nbclient"
  (nbclient has no such key) and never worked: timings reset
  `metadata.execution` before the timeout was read. It is read first now,
  with a test, and documented as Ganymede's own.
- Crate READMEs: the api crate's broadcast hub is gone, nbformat's
  big-int and cell-id gaps are fixed, contents has a validation message,
  and the server checks `Host`. The README's crate table lost its
  hand-kept test counts. `validation/` uses `ganymede serve`. The
  frontend README says the frontend isn't used yet, and that CI runs it
  on Node 24.

Already fixed before this pass: the iPad page's mtime-check wording, the
byte-identical claim (#79, #73), and the log's structure (#83).
