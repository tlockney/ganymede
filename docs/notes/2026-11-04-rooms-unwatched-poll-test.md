## 2026-11-04, test the unwatched room's poll path (#341)

`run_room` saves right after a poll that merged a change from disk, so a
room without a file watch still writes its edits to a file that never
settles. The existing never-settles test reached that branch only on
runners where watching failed.

A `Collab` with no root watches nothing, so its rooms poll:
`room_keeps_up_with_a_busy_file` takes a `watched` flag, and
`collaboration_room_saves_a_never_settling_file_while_polling` uses
`Arc::<Collab>::default()`.

Unwatched, that alone didn't pin the save: an unwatched room never sees
a disk event, so its ordinary autosave (1 s after the last edit) saved
the edit with the poll-path save deleted. The test's client therefore
keeps typing every 300 ms, so the autosave never comes due while the
file is busy and the poll-path save is the only one. With that save
deleted, or replaced by an edit mark, the test fails.
