## 2026-10-20, kernel start answers before the handshake (#173)

`POST /api/kernels` and `POST /api/sessions` waited for the startup nudge, so a slow kernel held the request for as long as it took to answer, and one that never answered held it for up to three launches of `startup_timeout` before a 500. jupyter_server answers once the process is launched: `_async_start_kernel` awaits `_finish_kernel_start`, which awaits the `KernelManager`'s `ready` future, and jupyter_client resolves that after `post_start_kernel`, before any `kernel_info_reply`. Ganymede now does the same. The kernel is in the map with `execution_state: starting` when the POST answers, and `boot` (the nudge and the relaunches) runs on a task that owns the lifecycle lock, so a shutdown or restart sent meanwhile waits for it. A kernel that never answers is reported `dead` and removed, as one that dies later is. A spawn failure (no such binary) still fails the POST. Restart still waits for the kernel's reply, as jupyter_server's does.

`KernelManager::wait_started` waits for a start or restart in progress; the unit tests use it where they relied on the old blocking start.

Lesson: when a choice is "deliberate" in the code, check the reference implementation's await chain before keeping it; here the comment explained the insertion order, not the request boundary.
