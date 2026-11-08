## 2026-11-08, A crashed kernel's running cell gets the error (#532)

`answer_for_crashed_kernel` numbered the unanswered shell messages with
`enumerate()` and then skipped those that aren't requests (no `_request`
suffix, such as `comm_msg`). A widget's `comm_msg` sent just before a cell
took index 0, so the cell that was running when the kernel died got
`aborted` and no `KernelDied` output: the user saw the cell stop with no
reason. The first message that is a request now gets the error.
