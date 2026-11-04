## 2026-11-04, retry a failed room save (#353)

`run_room` cleared its pending autosave (`last_edit`) after every save,
whatever `save_room` returned. A save that failed (three 412s in a row
against a busy writer, or an I/O error) left the room dirty with nothing
scheduled, and a later reload whose hash matched what the 412 path had
already read found nothing to save, so the edit waited for the next
edit or for the empty-room cleanup.

Every save now goes through `after_save`: success or a skip clears the
pending autosave; a failure schedules another one, with a backoff that
doubles while saves keep failing (1 s, 2 s, 4 s, ... up to about a
minute), so a full disk doesn't log a warning every second. The backoff
is a `last_edit` set in the future: `Instant::elapsed` saturates to zero,
so the autosave check waits out the backoff and then the save delay.

The test wraps the contents manager (`SaveLog` in `tests/http.rs`) and
fails the room's first save with an I/O error; without the change the
file never gets the edit.
