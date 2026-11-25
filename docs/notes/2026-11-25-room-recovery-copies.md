## 2026-11-25, room recovery copies and save state (#945)

A room could lose edits in two ways. An empty room closed after ten failed
saves in a row (or when its file was gone, or when another server owned the
file) with its edits only in the room. And a merge with the disk let the
disk win a cell both had changed, leaving the room's version in the log
line and nowhere else. The app half is ganymede-ui#792, not here.

What changed:

- `crates/ganymede-server/src/recovery.rs` (new): `write` keeps the room's
  notebook, as a save would write it, as `<id>.ipynb` with a `<id>.json`
  (`path`, `at`, `reason`, `detail`, `cells`, `notebook`), through a
  temporary file and a rename, notebook first. The id is 16 hex digits of
  microseconds, `-`, 8 random hex digits, so it sorts by time. `valid_id`
  accepts only that shape and every function takes an id through it, so an
  id is never a path. At most 20 copies of a path and 200 MB in all; a
  write over a limit deletes the oldest first and logs each, never the one
  just written. `list`, `read`, `discard`.
- `Collab::with_recovery_dir`; `serve.rs` sets it to `recovery` under
  `lab::data_dir()` (what `ganymede info` prints as `data_dir`: persistent,
  and not beside the notebook) whenever there is a data dir, embedding apps
  included. Without one (tests, no data dir) rooms behave as before.
- `run_room`: the three ways an empty room closes holding edits (file
  gone, saves keep failing, read-only with edits) write a copy first
  (`kept_before_closing`). If that fails the error is logged and the room
  stays, trying again on the next cleanup (`empty_since` reset). A copy
  made for a generation of the room's content isn't made twice if a client
  arrives during the close. `Cmd::Removed` (the file's id is gone) writes a
  `file_gone` copy too.
- `reload` takes the room's notebook before `merge_disk` when the room is
  dirty and a recovery dir is set; when the merge reports conflicts it
  writes that snapshot as a `merge_conflict` copy (`detail` the conflicting
  parts, `cells` the ids of "cell <id>" parts).
- Room `state` map, room-only (`Room::set_save_state`, `set_recovery`,
  `state_value` in ganymede-documents): `save` (`saved` or `failed`),
  `save_error`, `recovery` (`{"id","at","reason","cells"}`). `save_room` is
  now a wrapper over `try_save` that sets them from the result and sends
  the change to clients like any state change. A reload that leaves the
  room clean sets `saved`. A room opening on a path with copies names the
  newest. `saving` is not set: the save is one awaited call and clients
  would see it for an instant.
- Three routes on the `/api/collaboration` router, same auth layer:
  `GET /recovery?path=`, `GET /recovery/{id}`, `DELETE /recovery/{id}`. A
  discard sends every open notebook room a command that clears
  `state.recovery` if it names that id. No restore route: the client takes
  `content` and writes it through the contents API or the room.
- collab.rs now uses `tokio::time::Instant`, so a test can pause the
  clock; it is the same clock outside tests.

Network surface, justified: the apps need to show **Save needs attention**
with copy and restore, and a copy is files on the server's disk, which a
client on another machine can't reach. List and read are `GET`s that expose
notebook text the caller could already read through the contents API or the
room; discard deletes only files the server wrote in its own recovery dir.
The id check is the only thing between a request and a file name, so it is
exact and tested with `/`, `..`, `%2F` and other characters.

Decisions the brief left open (conservative choice each time):

- `Cmd::Removed` closes the room even when the copy can't be written: the
  file id is already dropped, and keeping a room that would save a deleted
  file back is worse. It logs an error. Every other close waits for a copy.
- A merge conflict whose copy can't be written has already merged. The
  room sets `save` to `failed` with an error saying the copy was not kept,
  and logs at error level; the edits are not recoverable.
- Copies are filed under the path they were made for. A rename leaves
  earlier copies under the old path; the discard route and `state.recovery`
  clearing work by id, so they still work.
- Shutdown (`flush_all`) writes no copy for a failed final save.

Tests (`recovery_tests` in collab.rs use paused time and a stand-in
contents that fails saves; the room's `Instant` is tokio's):

- `a_room_whose_saves_keep_failing_keeps_a_recovery_copy`: saves fail, the
  client leaves, ten failures pass: a `save_failed` copy with the typed
  source, parseable as a notebook, then the room retires.
- `a_room_whose_file_is_gone_keeps_a_recovery_copy`: `file_gone`.
- `a_room_that_cannot_keep_its_copy_stays_open`: the recovery dir is a
  file; the room is still open long after it would have closed; once the
  dir works, the copy is written and the room retires.
- `a_room_without_a_recovery_dir_still_gives_up`: no dir, as before.
- `a_merge_conflict_keeps_the_rooms_version`: copy holds the room's cell,
  `state.recovery` names it with `merge_conflict` and `["a"]`, the disk won
  the cell in the room.
- `a_merge_without_a_conflict_keeps_no_copy`.
- `save_state_follows_the_saves_and_stays_out_of_the_file`: `failed` with
  `save_error`, then `saved` with no `save_error`; none of the state keys
  in the `.ipynb`.
- `opening_a_room_names_the_newest_copy_until_it_is_discarded`.
- recovery.rs: id shape, a copy's round trip, 21 copies leave 20, the size
  bound.
- tests/http.rs: `collaboration_recovery_lists_returns_and_discards_copies`
  (list, get, delete, ids with `/`, `..`, `%2F`, other characters are 400,
  unknown 404), `collaboration_recovery_needs_the_same_auth_as_contents`
  (no or wrong token refused as `/api/contents` is, nothing deleted; 404
  without a recovery dir), and
  `collaboration_room_names_its_recovery_copy_until_it_is_discarded` (a
  websocket client sees `state.recovery` on open, and the discard removes
  it).
