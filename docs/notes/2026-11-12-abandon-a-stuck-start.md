## 2026-11-12, a kernel that never answers no longer holds up shutdown (#727)

The Mac app's in-app kernel once never loaded its page, so it never
answered `kernel_info_request`. Interrupt gave up after 10 s, but
Restart, a session's kernel change and quitting the app hung. The cause
was the lifecycle lock: a start holds it while it waits for the kernel's
first reply, which is up to `startup_timeout` (60 s) for each of three
launches, and shutdown, restart and the server's stop (`shutdown_all`)
all wait for that lock. A kernel change shuts the old kernel down, so it
waited too, and the app's quit waited on the server's stop.

Now each kernel has an `abandon_boot` counter. A shutdown (a client's
DELETE, a kernel change, the server's stop) or a restart bumps it before
it waits for the lock. `Core::boot` races its `nudge` against the counter
moving from the value it saw when it took the lock; when it moves, the
boot ends the session, leaves the kernel listed, returns an error, and
releases the lock, so the shutdown or restart goes ahead at once. A
restart reads the counter after taking the lock, so its own bump doesn't
cancel the boot it then runs. Culling idle kernels doesn't abandon a boot:
a booting kernel isn't idle.

A start nobody interrupts still fails as before: up to three launches,
then the kernel is reported dead and removed. A restart cancelled by a
shutdown answers 409.

Test: `a_kernel_that_never_answers_does_not_hold_up_shutdown_or_restart`
(`sessions.rs`) starts a kernel that stays connected and never answers,
then checks a shutdown, a restart, a session's kernel change and
`shutdown_all` each finish within 5 s. Each waited the full start before.

`restart_racing_shutdown_leaves_nothing_running` said the shutdown
waited for the restart to finish; now the restart may answer 409
instead, and the test accepts either.
