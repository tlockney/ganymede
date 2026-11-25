# Fix brief for #945 (pre-push review round 2)

Delete this file in your last commit. Don't push, don't use git stash,
don't touch anything outside this worktree. Commit with clear messages.
rustfmt/clippy may be missing here; format carefully by hand (rustfmt
style, 100 columns).

## H1: Pages check fails on docs/reference.md
docs/reference.md ~680: the `GANYMEDE_DATA_DIR` row ends with an inline
"(unreleased)". `node docs/scripts/check-unreleased.mjs` fails on it.
Drop it from the table cell and put the standard note under the table:

```md
> [!NOTE]
> **Unreleased.** Rooms keep recovery copies under this directory only
> in a build of `main` (HEAD), not in the latest release.
```
Run `node docs/scripts/check-unreleased.mjs` and make sure it passes.

## M1: test that clients receive the events
In `a_merge_conflict_keeps_the_rooms_version` and the save-state test
(`save_state_follows_the_saves...`) in collab.rs, read the `rx` that
`Fixture::join` returns, apply its Sync update frames to a client
`Doc` (as tests/http.rs does), and assert `state.recovery`,
`state.save` and `state.save_error` arrive at the client. Confirm each
fails when its dispatch is replaced with `let _ = ...` (the
`set_recovery` dispatch in `keep_lost` ~2765, in `kept_before_closing`
~2687, and `set_save_state("failed")` in `save_room`), then restore.

## M2: tests for the copy attempt before close
Add one test for `Cmd::Flush` (`flush_all()`) and one for `Cmd::Removed`
(collab.rs ~2076, ~2134): block the recovery dir, make a merge conflict
so a `lost` entry exists, unblock, then flush / remove, and assert a
`merge_conflict` copy is written. Each must fail with its
`|| !keep_lost(...)` removed.

## M3: copies visible across roots
Today `Store` keeps one dir per root (sha256 of the canonical root), so
a copy made by `ganymede mcp` rooted at a project is invisible to the
Mac app's server rooted at `/`. Change lookup to match by canonical
absolute file: `list`, `read` and `discard` scan all root dirs under
`recovery/` and keep copies whose `meta.file` is inside this server's
canonical root, then map them to this root's relative path. Writes can
stay in the per-root dir. Keep the size/count bounds per root dir. Never
expose `meta.file` in HTTP answers. Test with two Collabs on one data
dir, rooted at `/tmp/x` and `/tmp/x/proj` (tempdirs): a copy written by
the inner one is listed, read and discardable by the outer one at path
`proj/a.ipynb`, and the inner one still doesn't see an unrelated root's
copies. Update rooms.md ("a server sees only its root's copies") and the
dev log.

## L1: private permissions
On unix, create `recovery/` and each root dir with mode 0700
(`std::os::unix::fs::DirBuilderExt::mode`) and write copies 0600. Test
the modes under `#[cfg(unix)]`.

## L2: prune must keep the new copy
recovery.rs ~281-307: pass the new id to `prune` and never remove it
(skip it in the per-path count and the size loop). Test with a
pre-existing copy whose id is in the future (clock went back).

## L3: test the `say_saved` in `keep_lost`
collab.rs ~2781: test that `keep_lost` on a clean room with a writable
dir sets `save: "saved"`; must fail without that line.

## L4: iOS has no data dir
On iPad/iPhone `dirs::data_local_dir()` is None, so no copies. Say so in
docs/rooms.md and the dev log ("embedding apps included" is Mac only),
and say a follow-up issue tracks it (I'll file it; leave `#TBD` and I'll
replace it).

## L5: merge diagram
docs/images/rooms/merge.svg: change "file's source, cell logged" and
"deleted, cell logged" labels and the `aria-label` to mention the
recovery copy (e.g. "file's source, room's kept as a recovery copy").
Keep the SVG valid and the text fitting its boxes.

## L6: stale copy id
Two servers on one root share a store; a prune or discard on one leaves
the other's room naming a deleted id. In the recovery GET handler, on a
404 for the id named in a room's `state.recovery`, re-list and set the
room's `state.recovery` to the newest remaining copy (or clear it).
If that is more than a small change, document the gap in rooms.md
instead.

## Nits
- N1: rooms.md ~95-97 and collab.rs module doc ~29-34: add "except in
  the cases below".
- N2: rooms.md: "an id with other characters gets `400`" instead of
  "any other gets 400".
- N3: `reload` snapshots `room.notebook()` on every disk change while
  dirty. Only snapshot when the merge actually reports conflicts, if
  `merge_disk` can tell you before you need the snapshot; otherwise
  leave it and say why in a code comment.

## Checks
`cargo test -p ganymede-documents -p ganymede-server` and
`cargo test -p ganymede-cli --test recovery` must pass.
