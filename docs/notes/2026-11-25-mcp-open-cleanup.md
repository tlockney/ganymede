## 2026-11-25, A failed notebook_open removes what it made (#941)

#938 made a failed `notebook_open` leave no current notebook, but a
notebook the call had created stayed, and a kernel session it had started
kept running when the read after the start failed. See the #938 entry,
whose "Left for #941" this closes.

`notebook_open` now undoes what the call itself made when the kernel start
or the read fails:

- A session this call started is deleted, which shuts its kernel down. A
  session it joined (one already running for the path) is left alone. A
  failed delete is added to the error, except "not found" (someone else
  deleted it, and its kernel went with it). A session someone joins after
  this call started it is shut down with it; only one running before the
  call is left alone. This holds for an existing notebook too; the file is
  never touched then.
- A notebook this call created is deleted, but only if its stamp (size,
  modification time, inode) is still the one read right after the create.
  `Collab::delete_if_unchanged` re-reads the stamp and deletes under the
  same move lock and detached wrapper as `Collab::delete`, so ids and rooms
  follow. If the file changed (someone saved), the stamp can't be read (the
  contents aren't a local folder), or the delete fails, the file stays and
  the error says which: "`<path>` was created and stays (...), but no
  notebook is current, so pass path". When deleted: "...; the notebook was
  removed, no notebook is current, so pass path". Not "nothing was
  created": `create` may have made folders, which stay (deleting a folder
  could take what someone put there), and the message then says "the
  notebook was removed (the folders it made stay)".
- The stamp check matters because with `delete_to_trash` off a delete is
  permanent, and #938 kept the file because someone may have saved to it.
- It also stays when a room on the file has a client ("someone has it
  open"): a room saves 1 s after an edit, and deleting the file closes the
  room without saving, so a person's edit could be lost. This connection's
  own hold is released first. The room is looked up by the path's file id
  and asked directly; one that doesn't answer within two seconds (a slow
  save may hold unsaved edits) counts as in use, so the file stays. So
  does the session the call started, which is left running ("someone has
  it open; it and its kernel stay"). The file is decided first, under the
  move lock, and the session is shut down only if the result isn't "in
  use", so a client joining between a separate check and the delete can't
  leave the file kept and its kernel gone.
- A notebook this call created but whose session it joined (another client
  made one for the new path while it waited) is kept: "someone has a
  session on it". That kernel runs code against the file.
- The session id comes from `SessionManager::create_reporting`, which says
  whether the call made the session, so a session another client created
  while this call waited on the path lock is not taken for its own.

Known window that remains: the stamp is read after `create` returns, and
the checks and the delete run under the move lock, which keeps out renames,
deletes and new rooms but not a write to the file that doesn't go through a
room (another program, or a REST save of the file). A write landing from
the save in `create` until the delete is taken for the call's own (before
the stamp read) or deleted with the file (after it).
`Collab::delete` and `delete_if_unchanged` share one helper.

Tests: a kernel that won't start (file and session gone), a read failure of
a new notebook (the test provisioner overwrites the file after launch,
keeping its stamp), the same with a changed stamp (file stays, message
says why), a read failure of an existing notebook (session gone, file
kept), one with a joined session (still running), a delete that fails (the
file stays, "deleting it failed"), and collab unit tests for a changed
stamp, no local root, a client in the room, a client in another notebook's
room (no effect), and the ids going with the file. A gated test has
another client start the session while the open waits on the path lock
(it fails if the open takes that session for its own); an open of a nested
path (the folders stay and the message says so); contents without a local
root; a client from another connection holding the room when the kernel
won't start; a notebook created by the call whose session another client
made meanwhile (kept, with the session); and a room that doesn't answer (a
unit test with paused time). Not tested: a notebook someone has open whose
kernel started and whose read then failed (it can't be set up, since a room
won't open on a file that isn't a notebook), and a failing session delete
other than "not found". No test-only seam in the server was needed.
