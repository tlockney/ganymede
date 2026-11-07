## 2026-11-07, kernel failures stay out of crash reports (#431)

Crash reporting forwards every `error!` event from Ganymede's crates.
`ganymede-api`'s kernel manager logged a kernel that won't start, won't
restart, or keeps dying at `error`, so with reporting on each one went
to Sentry, with a kernelspec name or interpreter path in the message,
though docs/privacy.md says a kernel that exits is never reported.

Those four now log at `warn`. They are the user's code or environment,
and each is also in the kernel's own log (`/api/kernels/{id}/log`).
`kernel_log_outlives_a_kernel_left_dead` records the test's `tracing`
output and checks the failure shows as a warning and nothing logs an
error. The rule for later code: `error!` in Ganymede's crates means a
fault in Ganymede.
