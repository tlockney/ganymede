## 2026-11-07, session create, PATCH and DELETE races (#430)

`SessionManager` checked for a path's session, awaited the kernel start,
then inserted, so two `POST /api/sessions` for one notebook (two tabs, a
client retry) both missed the check and made two sessions and two
kernels. `create` also returned the existing session even when its
kernel had been culled or had died, and building its model then failed
with 404. A `PATCH` that changed the kernel inserted its copy of the
session back after the start, undoing a `DELETE` that ran meanwhile.

`create` and `update` now hold an async mutex across their kernel
starts. `create` drops a session whose kernel is gone and makes a new
one, as `jupyter_server` does. `delete` doesn't take the mutex; instead
`update` writes back only if the session still exists, and otherwise
shuts down the kernel it started and answers 404. Old kernels are shut
down after the mutex is released, so a slow shutdown doesn't hold up
other sessions.
