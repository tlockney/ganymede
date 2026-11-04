## 2026-11-04, test the unwatched room's poll path (#341)

`run_room` saves right after a poll that merged a change from disk, so a
room without a file watch still writes its edits to a file that never
settles. The existing never-settles test reached that branch only on
runners where watching failed.

No new knob was needed: a `Collab` built with no root watches nothing,
so its rooms poll. `room_keeps_up_with_a_busy_file` takes a `watched`
flag, and `collaboration_room_saves_a_never_settling_file_while_polling`
swaps in `Collab::new(FileIds::in_memory(), None)`. With the poll-path
save replaced by a plain edit mark, the test fails: the room never
reaches a quiet second to save in.
