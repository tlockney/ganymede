## 2026-11-25, A failed notebook_open no longer sets the current notebook (#938)

On a server with no kernelspecs, `notebook_open` on a new path created the
file, made it the current notebook, then failed to start a kernel. The agent
saw an error and thought nothing was open, while its next path-less
`cell_add` landed in that notebook.

`notebook_open` now sets the current notebook only after the kernel has
started and the notebook has been read. When it would create a notebook and
start a kernel, it resolves the kernel name first (the one named, the
folder default, the server default) and fails with "nothing was created"
when there is none; `--no-execute` and read-only still create without one.
If the kernel still fails to start for a notebook the call just created, the
file stays, and the error says so and says to pass `path`.
