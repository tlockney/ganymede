## 2026-11-25, A failed notebook_open no longer sets the current notebook (#938)

On a server with no kernelspecs, `notebook_open` on a new path created the
file, made it the current notebook, then failed to start a kernel. The agent
saw an error and thought nothing was open, while its next path-less
`cell_add` landed in that notebook.

`notebook_open` now behaves as follows:

- It sets the current notebook only after the kernel has started and the
  notebook has been read, which is the order it always had.
- Any failure clears the current notebook, including a path that doesn't
  end in `.ipynb`, so an earlier notebook doesn't stay current and receive
  the next path-less call. The agent passes `path`.
- When it would create a notebook and start a kernel, it resolves the kernel
  first (the one named, the folder default, the server default) and fails
  with "nothing was created" when there is none, or when the server default
  names a kernel that isn't installed. A kernel named in the call that isn't
  installed fails the same way ("no kernel named X; nothing was created").
  `--no-execute` still creates without one; read-only refuses to create.
- A failure leaves this connection holding no room and no read token that
  it didn't have before the call.
- If the kernel fails to start, or the read fails, for a notebook the call
  just created, the call deletes it and the error says nothing was kept.
  When the read failed after the call started a kernel session, that session
  goes too; a session that was already there, or that another call made, is
  left alone. The call first leaves the notebook's room itself, then asks
  that room whether anyone is in it. If someone is (a person who found the
  notebook during the kernel start), nothing is deleted and the error says
  "kept <path>: someone else has it open"; if the room doesn't answer in two
  seconds, it says "couldn't tell whether someone has it open" and keeps the
  file too. The check and the delete aren't atomic: a person opening the
  notebook in between loses it. If the delete itself fails, the error says
  the file remains.
- If it fails for an existing notebook, that stays as it was, and the error
  ends "no notebook is current, so pass path".
