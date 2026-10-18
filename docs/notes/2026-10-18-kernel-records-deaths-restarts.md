## 2026-10-18, kernel records freed, deaths reported, restart failures (#43, #44, #45)

Every kernel the API ever started stayed in memory: the task pumping a
session into the kernel's hub held the `RunningKernel`, which held the
session, which held the only sender of the channel the task waited on. A
kernel that died on its own was never noticed above the session: the model
said `idle`, no `status: dead` went out, and websockets stayed open. For
an attached kernel (`run --existing`, `Engine::connect`) nothing noticed at
all, because there is no process to wait on and the heartbeat had no caller.

- The pump holds a `Weak<RunningKernel>` and exits when the session dies
  (`KernelSession::watch_alive`/`dead`). A test checks the `Weak` is dead
  after shutdown and after a death.
- A session that dies while it is the kernel's current one, and no start,
  restart or shutdown is under way, removes the kernel from the map,
  publishes `status: dead` on the hub, and closes clients. That is what
  jupyter_server does when a kernel dies and is not restarted (its
  restarter would restart it first; auto-restart is still open). Clients
  drain the hub before they close, so they see the `dead` status. Sessions
  whose kernel is gone drop out of `/api/sessions`, as jupyter_server's
  "Kernel culled or died unexpectedly".
- Heartbeat: a session with no owned process pings every 3 s and is dead
  after 3 misses in a row. Misses count only after the first answer, so a
  kernel that is still binding its ports, or never answers heartbeats, is
  not declared dead by it. With ipykernel 7.4 attached through
  `run --existing`, killing the kernel mid-cell ends the run with
  `kernel died` 7 s later; before, it waited for the cell timeout.
- `mark_dead` closes the transport and `ZmqTransport` aborts its tasks on
  drop: a SUB socket otherwise keeps reconnecting to a dead kernel and
  `route` never ends. No deadline on `connect_retrying`: a closed transport
  stops it, and a deadline would race a slow kernel start.
- `execution_state` ignores statuses for jupyter_server's
  `untracked_message_types` (kernel_info, interrupt, shutdown, comm_info,
  history, debug, usage). An interrupt or a frontend's `kernel_info_request`
  mid-cell no longer shows a busy kernel as idle, which could let culling
  take it. The startup nudge's statuses are untracked too, so a successful
  start sets `idle` itself.
- Start and restart go through one `boot`: nudge, and on `KernelDied` or a
  timeout launch again on fresh ports, three tries, as the engine's
  `Relaunch::start` does (#25). A failed launch or a last failed nudge
  reports the kernel dead and removes it instead of leaving it "restarting".
  Twelve parallel `POST /api/kernels` with ipykernel all returned 201.
- A per-kernel async lifecycle lock is held across start, restart and
  shutdown, and each re-checks the map once it has the lock, so a restart
  racing a DELETE or cull either finishes first or gets a 404; it can no
  longer launch a process into an entry nobody owns.
- The control-channel interrupt waits 10 s for its reply: cdp-kernel waits
  up to 5 s for its target before replying, so the old 5 s reported a slow
  interrupt as failed.
