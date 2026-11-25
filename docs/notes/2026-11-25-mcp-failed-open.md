## 2026-11-25, A failed notebook_open no longer sets the current notebook (#938)

On a server with no kernelspecs, `notebook_open` on a new path created the
file, made it the current notebook, then failed to start a kernel. The agent
saw an error and thought nothing was open, while its next path-less
`cell_add` landed in that notebook.

`notebook_open` now behaves as follows:

- It sets the current notebook only after the notebook has been read and
  its kernel has started. It reads before starting the kernel, so a failed
  read leaves no session running (starting a kernel doesn't change the
  cells the read mark covers).
- Any failure after the path is validated clears the current notebook, so
  an earlier notebook doesn't stay current and receive the next path-less
  call. The agent passes `path`.
- When it would create a notebook and start a kernel, it resolves the kernel
  first (the one named, the folder default, the server default) and fails
  with "nothing was created" when there is none, or when the server default
  names a kernel that isn't installed. `--no-execute` still creates without
  one; read-only refuses to create.
- If the kernel then fails to start for a notebook the call just created,
  the call deletes that notebook (and any session) and the error says
  nothing was kept; if the delete fails, the error says the file remains.
- If the kernel fails to start for an existing notebook, it stays as it
  was, and the error ends "no notebook is current, so pass path".
