## 2026-11-03, Rooms interop test robustness (#290)

Follow-ups from #288's review, all in the `jupyter_ydoc` interop test:

- `e2e/collab/ydoc_client.py` bounds its HTTP call (`timeout=TIMEOUT`)
  and websocket open (`open_timeout`), and both skip `http_proxy`: an
  opener with an empty `ProxyHandler`, and `proxy=None` on `connect`
  (websockets 15 and later read proxy settings from the environment).
- `until()` stops as soon as the pump or send task has died, and says
  why: the task's exception, or the websocket's close code and reason.
  Before, a dead pump showed only as a timeout 20 s later.
- The cancelled tasks are awaited, so asyncio doesn't warn about tasks
  destroyed while pending.
- `collab.rs` prints its skip notice to stderr, fails when
  `GANYMEDE_E2E_PYTHON` is set but can't import the modules, and wraps the server
  in `KillOnDrop` before reading its first line, so a failed read still
  stops it. `KillOnDrop` moved into `tests/common`; `lab.rs`, `ui.rs` and
  `lab_start.rs` had their own copies.
- The three CI jobs that install the oracle set `GANYMEDE_E2E_PYTHON` to
  the `python` they installed it into, so a broken install fails with the
  import error instead of a skip (which the e2e job's "Nothing skipped"
  step would also catch, without saying why).
