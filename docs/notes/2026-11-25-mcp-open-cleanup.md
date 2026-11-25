## 2026-11-25, A failed notebook_open removes what it made (#941)

#938 made a failed `notebook_open` leave no current notebook, but a
notebook the call had created stayed, and a kernel session it had started
kept running when the read after the start failed. See
[the #938 entry](2026-11-25-mcp-failed-open.md), whose "Left for #941" this
closes.

`notebook_open` now undoes what the call itself made when the kernel start
or the read fails:

- A session this call started is deleted, which shuts its kernel down. A
  session it joined (one already running for the path) is left alone. A
  failed delete is added to the error. This holds for an existing notebook
  too; the file is never touched then.
- A notebook this call created is deleted, but only if its stamp (size,
  modification time, inode) is still the one read right after the create.
  `Collab::delete_if_unchanged` re-reads the stamp and deletes under the
  same move lock and detached wrapper as `Collab::delete`, so ids and rooms
  follow. If the file changed (someone saved), the stamp can't be read (the
  contents aren't a local folder), or the delete fails, the file stays and
  the error says which: "`<path>` was created and stays (...), but no
  notebook is current, so pass path". When deleted: "...; nothing was
  created, no notebook is current, so pass path".
- The stamp check matters because with `delete_to_trash` off a delete is
  permanent, and #938 kept the file because someone may have saved to it.

Known window: a room's autosave doesn't take the move lock, so a save that
lands between the stamp check and the delete is deleted with the file. The
window is tiny and the file was created moments before by this call.

Tests: a kernel that won't start (file and session gone), a read failure of
a new notebook (the test provisioner overwrites the file after launch,
keeping its stamp), the same with a changed stamp (file stays, message
says why), a read failure of an existing notebook (session gone, file
kept), and one with a joined session (still running). No test-only seam in
the server was needed.
