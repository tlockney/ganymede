## 2026-11-04, keep a room whose save fails (#359)

`run_room`'s empty-room cleanup saved a dirty room and then retired it
whatever the save returned. With #353's retry, that was the one path
where a failed save still dropped edits: once the cleanup delay passed
with no clients and an idle kernel, the room closed with them unsaved.

The cleanup now retires a room only when it is clean, with two
exceptions. It saves only when no autosave is due, so a failing save has
one retry clock, #353's backoff, rather than the cleanup's own save
pushing the autosave back each delay. While saves fail, the room stays
open and warns each cleanup delay. It closes anyway, with a warning that
names the path, when its file is gone (a delete or move outside the
contents API, which `Collab::removed` never hears of) or after ten
failed saves in a row, about five minutes of backoff. Before those
exits, a file that could never be saved (deleted, read-only, or content
that isn't a notebook) kept its room, watch and warnings for the life of
the process.

Tests: `room_edit_with_failing_saves` is the #353 retry test's setup,
shared. `collaboration_room_outlives_cleanup_while_its_save_fails` drops
the client right after its edit, with a 100 ms cleanup delay and two
failing saves; the autosave's retries save it, then the room retires.
`collaboration_room_without_a_file_retires_with_unsaved_edits` deletes
the file outside the API and checks the room still retires.
`Collab::open_rooms` lets tests see that.
