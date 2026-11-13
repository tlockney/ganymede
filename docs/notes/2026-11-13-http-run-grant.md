## 2026-11-13, HTTP runs get their notebook's grant (#745)

`POST /api/notebooks/run` starts the run's kernel with a working
directory but no session, so the host API found no notebook for it and
answered with the `[host]` defaults: a notebook narrowed to
`read = ["reports"]` could read the whole root when run from a link with
parameters, the apps' Run Notebook or an HTTP client, and one whose
grant added `run` lost it. The host API's own `notebooks.run` was right:
it registers the run's grant under `GANYMEDE_HOST_RUN`.

`run_notebook` now also sets `GANYMEDE_HOST_NOTEBOOK`, the notebook's
file id, in the kernel's launch variables, which the server keeps and
the kernel can't change. `HostApi::who` reads it after
`GANYMEDE_HOST_RUN` (a host run keeps its narrowed grant), looks up the
id's current path and answers with `grant_for(Some(path))` and
`edit_self` off, as `notebooks.run` does, since a run writes at most a
copy. A file id rather than the path, so a rename during a long run
keeps the notebook's grant and another notebook moved to the old path
doesn't lend the run its own; a deleted notebook leaves the run with no
grant. The id is made only once the notebook is read, under the move
lock (`Collab::index_existing`), so a run of a path that isn't there
leaves no id behind, and `who` reads the path and the grant by id in one
file-id call, so no rename lands between them. A launch variable rather than
`HostApi::pin`, because the HTTP route has the app state but not the
host API.
