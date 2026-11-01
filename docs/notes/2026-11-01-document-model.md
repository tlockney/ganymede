## 2026-11-01, document model: watcher, merges, file ids (#141)

Roadmap step 6, on the rooms from the spike (#243, the yrs document
rooms entry of 2026-10-31): items 1, 2 and 5 of its plan.

- **Watcher.** `ganymede-server/src/watch.rs`: one `notify` watcher per
  server, on each open file's parent directory, not the file, so a save by
  temp file and rename (ours and most editors') is still seen. Access
  events (inotify's `IN_ACCESS`) are dropped. A room debounces events by
  500 ms and then reads the file; a hash equal to its own last save is
  ignored. It still polls every 10 s, in case a file system drops events,
  and every second where there are no events: iOS and other targets build
  without `notify`, and the embedded server has no root on disk.
- **Merge, not "disk wins".** The spike reloaded from disk when the file
  changed, losing unsaved room edits. Now `ganymede_documents::merge` does
  a three-way merge by cell id, with the file as last read or written as
  the base: one-sided changes, additions (kept after the cell they
  followed) and deletions all apply; a cell both sides changed, or one
  deleted and the other changed, goes to the disk, and the server logs it.
  Cell order: whichever side kept the base order of the shared cells
  takes the other's; two different reorders take the disk's. A save
  refused with 412 merges and retries, up to three times. Notebooks
  before 4.5 have no ids and still take the disk outright.
- **Comparing as JSON.** The merge compares cells as `serde_json` values
  of the `Notebook`, the same form `schema::set` writes, so a cell
  round-tripped through Yjs compares equal to the file's.
- **File ids** (`ganymede_contents::FileIds`): SQLite at
  `<jupyter data dir>/ganymede_file_ids.db`, keyed by root and API path,
  not `jupyter_server_fileid`'s database (its schema tracks inodes, and
  we don't want two writers on it). `PATCH` and `DELETE` on contents move
  or drop ids for a file or a directory's subtree, and open rooms take the
  new path (`state.path`) and re-watch. A file moved by another program
  gets a new id when next opened.
- **Not done:** one owner per file across processes (#272). With merges,
  two servers on one file converge through it rather than overwrite each
  other, so this is less urgent than it was.

`notify` 8 is CC0-1.0; on Linux it adds `inotify` (ISC), `mio`, `walkdir`
(Unlicense/MIT); on macOS `objc2-core-services`. All are in `about.toml`'s
accepted list.
