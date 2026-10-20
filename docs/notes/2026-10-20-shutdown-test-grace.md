## 2026-10-20, shutdown escalation test gives the TERM trap time to run (#212)

`shutdown_escalates_to_sigterm_then_sigkill` shut its shell "kernel" down with a 400 ms grace, so SIGKILL followed SIGTERM after 200 ms. A shell runs its `trap ... TERM` only when it is scheduled again, and on a loaded macOS runner that took longer: SIGKILL landed first and the file never said "term". The test now uses a 2 s grace (1 s between the signals) and bounds the whole shutdown by 2 x grace plus slack. The escalation order itself is checked without timing by the fake-process unit test `shutdown_sends_sigterm_before_sigkill`.

Lesson: a real-process test that waits on a signal handler needs a window measured in seconds, not the few hundred milliseconds a fake process needs.
