## 2026-11-01, rooms against a jupyter-collaboration client (#258)

`crates/ganymede-cli/tests/collab.rs` runs `e2e/collab/ydoc_client.py`
against `ganymede serve`: `jupyter_ydoc` 4.2.0's `YNotebook` on `pycrdt`
0.14.10, with `pycrdt`'s sync messages over `websockets`, which is what
`jupyter-collaboration` builds its server and Python clients on. Python
is a test oracle, as in the rest of the e2e job.

- The client syncs, reads the same cells as the file, then edits a cell's
  `source` Text and appends a cell through `YNotebook.append_cell`, while
  the script also rewrites the file with a change to another cell. The
  room merges the disk change (#141), saves, and the client and the file
  end equal with all three changes. Passed five runs in a row locally,
  about 2.5 s each.
- `jupyter_ydoc` reads the room without complaint: our schema (no
  `language_info` or `kernelspec` defaults, integers kept as integers) is
  one it accepts.
- The client sends its own updates only, not echoes of what it applied,
  by observing the doc with a flag set while applying the server's.
- Skips with `skipping:` when no Python has the three modules, so the e2e
  job's "Nothing skipped" check makes it mandatory there; the job now
  installs them, pinned in `E2E_PYTHON_PACKAGES`.
