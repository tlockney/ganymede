## 2026-11-09, JPY_SESSION_NAME is the notebook's path (#623)

The kernel host set `JPY_SESSION_NAME` to the kernel id for every
kernel. jupyter_server sets it, for a session's kernel, to the kernel's
directory for the session path joined with the session name
(`SessionManager.get_kernel_env`), and updates it on a rename for the
next restart. ipykernel reads it for `__session__`, and it is the only
way kernel code can find the notebook it runs in, which the
notebooks-from-notebooks research needs first.

`KernelManager::start_for_session` takes the value and keeps it on the
kernel, so a restart (asked for or after a crash) passes it again.
`SessionManager::update` stores the new value on a path or name change
without restarting. The host still defaults to the kernel id, and a
launch's `env` now overrides it. A kernel started with
`POST /api/kernels` keeps the id: jupyter_server sets nothing there, and
dropping the variable could break a kernel that reads it.
