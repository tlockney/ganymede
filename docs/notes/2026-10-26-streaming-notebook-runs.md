## 2026-10-26, Streaming notebook runs and stdin (#158)

The notebook runner collected a cell's messages and applied them only after the reply, so a host running a notebook saw nothing until each cell ended, and never answered `input_request`. The engine's single-cell `execute_with` already streamed and answered input, but its outputs came from a fresh `NotebookRun` per call, so a display update aimed at an earlier cell was lost.

The streaming loop moved from `ganymede-engine` into `ganymede-exec` as `execute_streaming`, and `Handler`/`NoHandler` moved with it (the engine re-exports them, so its paths are unchanged). `NotebookRun` now applies each message as it arrives and reports it through a `NotebookHandler`; a closure taking an `Event` still works, through a blanket impl. `Options::allow_stdin` defaults to off, as `nbclient` does, and `ganymede execute` keeps it off.

Lesson: the timeout policy differs between the two callers. The engine interrupts a cell that times out; `nbclient` (and so the notebook runner) does not, by default. `execute_streaming` returns the timeout and leaves the choice to the caller.
