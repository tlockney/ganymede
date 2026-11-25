## 2026-11-25, A failed notebook_open no longer sets the current notebook (#938)

On a server with no kernelspecs, `notebook_open` on a new path created the
file, made it the current notebook, then failed to start a kernel. The agent
saw an error and thought nothing was open, while its next path-less
`cell_add` landed in that notebook.

`notebook_open` now behaves as follows:

- It sets the current notebook only after the kernel has started and the
  notebook has been read. The kernel-then-read order is unchanged; what
  changed is that `current` is set after both, where it used to be set
  before the kernel start.
- Any failure clears the current notebook, including a path that doesn't
  end in `.ipynb`, so an earlier notebook doesn't stay current and receive
  the next path-less call. The agent passes `path`.
- Before it writes anything it checks there is a kernel to start: the one
  named, the folder default, or the server default. With none, or with a
  named kernel or a server default that isn't installed, it fails with
  "nothing was created". `--no-execute` still creates without a kernel;
  read-only refuses to create.
- If the kernel then fails to start, or the read fails, for a notebook the
  call just created, the file stays (someone may have saved to it by now;
  an earlier version deleted it and could lose that work) and the error says
  so: "<path> was created and stays, but no notebook is current, so pass
  path".
- If it fails for an existing notebook, that stays as it was, and the error
  ends "no notebook is current, so pass path".
- A failure leaves this connection holding no room that it didn't hold
  before the call. (A read that fails may leave an unused read token for the
  path; the next read reuses it.)

Left for a follow-up issue, filed once this merges: when the kernel fails to
start after the notebook was created, the file stays rather than being
removed; and a read that fails after the kernel started leaves that kernel's
session running, as it did before.
