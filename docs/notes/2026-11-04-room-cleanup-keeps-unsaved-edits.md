## 2026-11-04, keep a room whose save fails (#359)

`run_room`'s empty-room cleanup saved a dirty room and then retired it
whatever the save returned. With #353's retry, that was the one path
where a failed save still dropped edits: once the cleanup delay passed
with no clients and an idle kernel, the room closed with them unsaved.

The cleanup now settles its save through `after_save`, like every other
save, and retires the room only when it is clean afterwards. A room that
is still dirty stays open, logs a warning with the path each cleanup
delay, and the autosave keeps retrying with #353's backoff. A file that
can't be saved at all (a full disk that stays full) keeps its room in
memory rather than losing the edits; a client that reconnects finds them.

The retry test's setup is now `room_edit_with_failing_saves`, shared by
`collaboration_room_outlives_cleanup_while_its_save_fails`, which drops
the client right after its edit, with a 100 ms cleanup delay and two
failing saves. Without the change the room retires and the file never
gets the edit.
