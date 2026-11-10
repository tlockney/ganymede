## 2026-11-10, registering an app's kernels through the C ABI (#266)

Built Ganymede's half of [In-app kernels as server kernels](../in-app-kernel.md):
`ganymede_kernel_register`, `ganymede_kernel_deliver` and
`ganymede_kernel_ended`, over a new `RegisteredKernels` in
`ganymede-kernel-host` that is a kernelspec source, provisioner and
transport factory at once. The server puts it in front of whatever
kernels it would otherwise have (`Startup::registered`), so the iPad's
in-app kernel can sit beside a gateway's.

Two things the seams didn't give for free. The transport factory gets
only a `LaunchedKernel`, with no kernelspec name, so `RegisteredKernels`
recognises its own launches by the address of the process handle it
made, and passes anything else to the fallback. And a kernel the
platform ended needed a way to say so: `KernelProcess` gained
`exit_reason()`, which the crash path reads to restart a `"lost"`
kernel without counting it toward `restart_limit` and to add
`"reason": "lost"` to the `status: restarting` it announces.

Messages from the app go into an unbounded queue, so `deliver` never
blocks the app's thread, and a task moves them into the session's
bounded one. The test in `crates/ganymede-ffi/tests/server.rs` drives a
fake kernel through the C functions over the REST API: a session starts
it with the notebook's path, `"lost"` restarts it under the same id, a
DELETE stops it, and `release` runs once at server stop. The apps' side
is ganymede-ui's next step.
