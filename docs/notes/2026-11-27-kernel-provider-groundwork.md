## 2026-11-27, kernel provider groundwork (#984)

`RegisteredKernels` now knows where a spec came from: an app's, through the
C ABI, still stands in for a fallback's namesake, while a provider's may not
take a name the fallback lists, and is listed after everything else and never
launched in place of a fallback's namesake. `unregister(name)` forgets a spec
without touching its running kernels, and `end_all_from` ends one app's
kernels and drops its specs, which is what the kernel provider socket will
call when a connection closes. A start whose app finishes it later now has
`AppKernel::started`, a receiver `launch` waits on for the app's answer,
failing and cleaning up like any other failed start. That wait lasts at most
the launch's startup timeout, a launch dropped while it waits stops the kernel
the app started, and a kernel a provider started is relaunched through its
provider or not at all, never through the fallback. A new `Ended::Gone` reads
as `lost` to clients but makes the process answer
`KernelProcess::restartable() == false`, so ganymede-api leaves that kernel
dead rather than restarting it, while a platform loss still restarts within
its own limit. `ganymede serve` builds a `RegisteredKernels` in front of its
own kernels even with no embedding app, so the provider socket has somewhere
to register; the socket itself, with its config and listener, is #983.
