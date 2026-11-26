## 2026-11-25, room recovery copies and save state (#945)

A room could lose edits in two ways. An empty room closed after ten failed
saves in a row (or when its file was gone, or when another server owned the
file) with its edits only in the room. And a merge with the disk let the
disk win a cell both had changed, leaving the room's version in the log
line and nowhere else. The app half (showing, restoring and discarding
these copies) is ganymede-ui#831, not here.

What changed:

- `crates/ganymede-server/src/recovery.rs` (new): `write` keeps the room's
  notebook, as a save would write it, as `<id>.ipynb` with a `<id>.json`
  (`path`, `at`, `reason`, `detail`, `cells`, `notebook`), through a
  temporary file and a rename, notebook first. The id is 16 hex digits of
  microseconds, `-`, 8 random hex digits, so it sorts by time. `valid_id`
  accepts only that shape and every function takes an id through it, so an
  id is never a path. At most 20 copies of a path and 200 MB in a root's dir; a
  write over a limit deletes the oldest first and logs each, never the one
  just written. `list`, `read`, `discard`.
- Each root has its own dir in the recovery dir, the hex SHA-256 of its
  canonical path (`recovery::Store`), and that is where a server writes.
  Reads match by the copy's file, not its dir: `list`, `read` and
  `discard` scan every root dir under `recovery/` and keep the copies
  whose `file` is inside this server's canonical root, mapped to that
  root's relative path. Every server of a person shares the data dir and
  `path` is relative to a root, so this is what keeps one project's
  copies from being offered in another — and what makes a copy kept by
  `ganymede mcp` rooted at a project visible to the Mac app's server
  rooted at `/`, which shares the data dir. Writes and the bounds stay
  per root dir. A store with no root (tests, contents that are not a
  local dir) sees its own dir only, as before. The meta also has the
  file's absolute path (`file`), which the routes leave out.
- Ids strictly increase within a process: a clock that stands still or
  goes back gives the next microsecond.
- `Collab::with_recovery_dir`; `serve.rs` sets it to `recovery` under
  `lab::data_dir()` (what `ganymede info` prints as `data_dir`: persistent,
  and not beside the notebook) whenever there is a data dir, embedding
  apps included. On iPad and iPhone `dirs` reads `$HOME`, which iOS sets
  to the app container, so copies go under its `Library/Application
  Support/ganymede`; #957 tracks checking that on a device. Without
  a data dir (tests), or when the dir can't be
  made (a warning is logged), rooms behave as before. Only `recovery` is
  made at start; a root's dir is made by its first copy.
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
  parts, `cells` the ids of "cell `<id>`" parts).
- Room `state` map, room-only (`Room::set_save_state`, `set_recovery`,
  `state_value` in ganymede-documents): `save` (`saved` or `failed`),
  `save_error`, `recovery` (`{"id","at","reason","cells"}`). `save_room` is
  now a wrapper over `try_save` that sets them from the result and sends
  the change to clients like any state change. A reload that leaves the
  room clean sets `saved`. A room opening on a path with copies names the
  newest. `saving` is not set: the save is one awaited call and clients
  would see it for an instant; the app shows "Saving" itself (ui#808).
  A read-only room with edits says `failed`, "another server owns the file".
- Three routes on the `/api/collaboration` router, same auth layer:
  `GET /recovery?path=`, `GET /recovery/{id}`, `DELETE /recovery/{id}`. A
  discard sends every open notebook room a command that, if
  `state.recovery` names that id, names the next newest copy of the path,
  or none. So does each copy that `prune` deletes. No restore route: the client takes
  `content` and writes it through the contents API or the room.
- collab.rs now uses `tokio::time::Instant`, so a test can pause the
  clock; it is the same clock outside tests.

Network surface, justified: the apps need to show **Save needs attention**
with copy and restore, and a copy is files on the server's disk, which a
client on another machine can't reach. List and read are `GET`s that expose
notebook text the caller could already read through the contents API or the
room, since a server sees only the copies of files under its own root;
discard deletes only copies whose file is under that root, whichever
root's dir they are in.
The id check is the only thing between a request and a file name, so it is
exact and tested with `/`, `..`, `%2F` and other characters.

Decisions the brief left open (conservative choice each time):

- `Cmd::Removed` closes the room even when the copy can't be written: the
  file id is already dropped, and keeping a room that would save a deleted
  file back is worse. It logs an error. Every other close waits for a copy.
- A merge conflict whose copy can't be written has already merged. The
  room holds its notebook from before (`Disk::lost`, at most 20), logs an
  error and keeps `save` at `failed` through later merges and saves. It
  tries again at each save and before closing, and stays open until the
  copy is written. A client's explicit save that succeeds ends it too:
  that is read as the person acting.
- A room that isn't a notebook can't be copied. It closes after
  `GIVE_UP_AFTER` failures with an error in the log, and when the file
  replaces it, `save` is `failed` with the reason until a client saves.
- `Cmd::Removed` is also how a deletion the room's own look finds
  (`file_gone`) ends, so that closes without a copy too, like a delete
  through the contents API. Only the cleanup's own check keeps the room.
- Copies are filed under the path they were made for. A rename leaves
  earlier copies under the old path; the discard route and `state.recovery`
  clearing work by id, so they still work.
- Shutdown (`flush_all`) writes a copy of a room still dirty after its
  last save, `read_only` or `save_failed`, and logs an error if it can't.

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
- `opening_a_room_names_the_newest_copy_until_it_is_discarded`: then the
  next newest, then none.
- `edits_lost_without_a_copy_stay_failed_until_they_have_one`,
  `a_clients_own_save_clears_edits_lost_without_a_copy`,
  `a_save_that_loses_edits_does_not_clear_them`,
  `a_room_with_lost_edits_stays_open_until_they_have_a_copy`,
  `a_reload_that_leaves_the_room_clean_says_saved`.
- `a_replaced_room_that_is_no_notebook_says_so`,
  `a_room_that_is_no_notebook_still_gives_up`.
- `stopping_keeps_a_copy_of_edits_that_cannot_be_saved`,
  `stopping_keeps_a_copy_of_a_read_only_rooms_edits`.
- `a_read_only_room_keeps_a_copy_before_closing`,
  `a_cleanup_that_finds_the_file_gone_keeps_a_copy`,
  `closing_again_keeps_no_second_copy_of_the_same_edits`.
- `a_room_stops_naming_a_copy_that_is_pruned`,
  `servers_on_two_roots_offer_only_their_own_copies`,
  `a_recovery_dir_that_cannot_be_made_is_done_without`.
- recovery.rs: id shape, `ids_strictly_increase`, a copy's round trip,
  `roots_do_not_see_each_others_copies`, 21 copies leave 20, the size
  bound (measured from the copies, whose sizes differ with their times).
- `crates/ganymede-cli/tests/recovery.rs`: `ganymede serve` offers the
  copies in its data dir.
- tests/http.rs: `collaboration_recovery_lists_returns_and_discards_copies`
  (list, get, delete, ids with `/`, `..`, `%2F`, other characters are 400,
  unknown 404), `collaboration_recovery_needs_the_same_auth_as_contents`
  (no or wrong token refused as `/api/contents` is, nothing deleted; 404
  without a recovery dir), and
  `collaboration_room_names_its_recovery_copy_until_it_is_discarded` (a
  websocket client sees `state.recovery` on open, and each discard moves it
  on), `collaboration_recovery_is_per_root`.

Review round 2, same day:

- docs/reference.md: the `GANYMEDE_DATA_DIR` row's inline "(unreleased)"
  moved to the standard note under the table, which check-unreleased.mjs
  accepts and a release finds.
- A room's state changes reach its clients as updates, not only its own
  copy of the state map. The tests read the client side of the room
  (`client` in `recovery_tests`): a doc that answers the room's sync
  step 1 and applies its frames, as tests/http.rs's websocket client
  does, until the state it was owed arrives. `state.recovery` and
  `save: "saved"` in the merge conflict test, `save`/`save_error`
  through a failed and a good save in `save_state_follows...`, and the
  copy named at shutdown in `stopping_keeps...`. Each fails with the
  `dispatch` it rides replaced by `let _`: `set_recovery` in
  `keep_lost` and in `kept_before_closing`, `set_save_state("failed")`
  in `save_room`.
- `Cmd::Flush` and `Cmd::Removed` try again the copy a merge conflict
  couldn't write, once the dir works:
  `stopping_writes_the_copy_a_merge_conflict_lacked` and
  `removing_the_file_writes_the_copy_a_merge_conflict_lacked`. Each
  fails with its `|| !keep_lost(...)` removed.
- `keep_lost` says `saved` on a clean room whose copy it just wrote:
  `keep_lost_on_a_clean_room_says_saved`, which fails without the
  `say_saved` in it.
- Two servers on one root share the store, so a discard or a prune on
  one leaves the other's room naming a copy that is gone. A 404 read
  now asks every open notebook room whether `state.recovery` names the
  id (`Collab::recovery_gone`, from the GET route), and each that does
  names the newest copy of its path left, or clears the key.
  tests/http.rs deletes the copies from under the server, as the other
  server would, and reads them: the room names the next copy, then,
  with none left, clears the name.
- rooms.md and the collab.rs module doc say the
  no-closing-without-a-copy rule has the exceptions each lists below
  it. The id rule reads "an id with other characters gets 400". The
  merge diagram's two warn boxes and its alt text name the recovery
  copy. rooms.md says iPad and iPhone keep copies in the app container,
  with #957 tracking a check on a device.
- `reload` still snapshots the room's notebook on every disk change
  while it is dirty: `merge_disk` applies the merge and reports the
  conflicts in one call, so the room's own version is gone by the time
  the conflicts are known, and telling beforehand would take a merge of
  its own over a notebook serialized like the snapshot. The comment at
  the snapshot says so.
