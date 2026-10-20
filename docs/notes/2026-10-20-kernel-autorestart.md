## 2026-10-20, crashed kernels restart under the same id (#144)

`KernelManager` now does what `jupyter_client`'s `KernelRestarter` does for jupyter_server: a kernel whose process exits with nothing asking it to is relaunched under the same id and cwd, clients see `status: restarting` (JupyterLab treats that as an automatic restart and reconnects), and after `restart_limit` crashes in a row (default 5, each within 10 s of the last restart) it is reported `dead` and removed as before. `kernel_restart_limit` / `--kernel-restart-limit` sets it; 0 keeps the old behavior.

Two rules from nteract's daemon on top of jupyter_server's:
- An exit the server or a client asked for is not a crash: API shutdown and restart already went through `booting` and map removal, and now a client's own `shutdown_request` (with or without `restart`) marks the session as asked to exit. The restart flag case matters for `ganymede-engine` through a gateway, which sends `shutdown_request{restart: true}` and then launches the next kernel itself; restarting on the remote too left two kernels for a moment.
- Requests a crash leaves unanswered get answers. The manager remembers each client's shell requests until the shell reply arrives; on a crash the oldest (the one running) gets an `error` reply with `ename: KernelDied` and, for a cell, an `error` output, and the rest get `status: aborted`, each followed by `status: idle`. jupyter_server answers nothing, so its clients wait until JupyterLab's own restart handling clears them.

To restart from the pump task, the launch state (provisioner, transports, kernel map, config) moved into an `Arc<Core>` the pump holds weakly; `KernelManager::config` is now a method.

Observed while writing the e2e test: after a cdp-kernel crash jupyter_server keeps reporting the kernel `busy` (the crashed cell's last status), because the new cdp-kernel publishes no parentless status and the startup `kernel_info` statuses are untracked. Ganymede's boot sets it `idle` once the new kernel answers. The test only waits for idle on Ganymede.

Lifecycle versus output (runt's ADR): death is a `watch` channel separate from the message broadcast in both the session and the pump, and interrupts go to the kernel without passing through any output queue, so an output flood delays neither.
