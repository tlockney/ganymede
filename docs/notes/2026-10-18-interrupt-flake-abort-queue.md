## 2026-10-18, interrupted prompt flake was ipykernel's abort queue (#137)

`input_without_stdin_and_interrupt_while_prompting` failed on macOS with an empty result for the cell after the interrupt. Under CPU load it reproduced on Linux too (3 of 40 runs). The failing cell's reply was `{"status": "aborted"}` with no output: the kernel never ran it.

Cause: the interrupted cell ends in an error, and with `stop_on_error` (the default) ipykernel 7 sets an aborting flag and posts a "stop aborting" marker through its shell channel thread. Any `execute_request` that reaches the subshell before that marker is answered `aborted`. A client that sends the next cell as soon as the error reply arrives can win that race on a slow machine. This is kernel behaviour, not a server bug; jupyter_client clients see the same thing.

Fix: the test sends the interrupted cell with `stop_on_error: false`, so no abort window opens. Queue aborts stay covered by `stop_on_error_aborts_queued_cells`. 80 of 80 runs passed under the same load.

Lesson: a cell that comes back `aborted` after an error is ipykernel's stop-on-error window, so a test that follows an error with another cell should either clear `stop_on_error` or expect the abort.
