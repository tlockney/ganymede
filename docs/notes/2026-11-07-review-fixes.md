## 2026-11-07, five fixes from a full code review (#424–#428)

A review of the whole workspace after 0.7.0 filed #424–#440. This entry
covers the five fixed first: the ones that let one request crash the
server or reach outside the root, or that lose kernels.

- **Checkpoints and symlinks (#424).** `check_inside_root` guarded every
  contents path except the checkpoint's, and `atomic_write` follows
  links on purpose (it replaces a link's target, not the link). A
  `.ipynb_checkpoints` entry linking out of the root turned a checkpoint
  into a write anywhere, and a restore into a read of anything.
  `jupyter_server` follows those links too; ours now don't, since
  `security.md` promises the root. The walk-up-and-canonicalize check is
  now `resolves_under(base, path)`, used for the root and for
  `--checkpoint-dir`. The review of the first version found the
  best-effort upkeep too: deleting or renaming a file removed or moved
  its checkpoint through the link. Those now leave such a checkpoint
  alone, and listing shows none rather than failing, since JupyterLab
  lists on every open.
- **Allocation failure is an abort, not a panic (#427).** A v3 heading's
  `level` went straight into `"#".repeat(level)`. Python raises
  `MemoryError`; Rust aborts the process, and `spawn_blocking` can't
  catch that. Clamped at 1024.
- **Stack overflow is an abort too (#428).** Two kinds of nesting:
  - yrs 0.28's `Any::decode` recurses per level with no limit, at two
    bytes a level, and a 1 GiB message limit let a client nest as deep as
    it liked. There is no yrs option to bound it, so `depth.rs` walks a v1
    update iteratively, along `Update::decode`, `ItemContent::decode` and
    `Any::decode`, and the room refuses one deeper than 100. A malformed
    update passes the scan and yrs reports it.
  - Shared types nested in one another, each level its own item and
    possibly its own update, so a scan of one update can't see the depth.
    yrs deletes them recursively (`TransactionMut::delete`), so a tree
    that is in the document can never be taken out again: the review
    reproduced the abort by deleting one. An update that creates shared
    types is applied to a scratch copy of the document first (its state
    encoded with pending updates, the update on top) and measured there
    with an iterative walk; past 16 levels it is refused. The notebook
    schema goes 5 deep. Typing creates no shared types, so the copy is
    made when a cell is added, not per keystroke.
  - 100 + 16, plus the notebook's own levels, stays under the 128 levels
    `serde_json` reads back, so a room can't save a file Ganymede can't
    open. `out_to_json` stops at 16 as a backstop and gives null for XML
    types, whose yrs conversion recurses without limit.
  - The collaboration websocket's 1 GiB message limit is left to #436.
- **A beat cut short (#425).** Two timeouts of the same length, nested:
  the outer one always wins, and dropping the inner future skipped the
  code that replaced the stuck REQ socket. zeromq's REQ refuses to send
  again until it reads a reply, so one missed beat became every missed
  beat. The transport now notices a beat dropped part way
  (`awaiting_echo` still set) and bounds the whole beat itself.
- **Remote kernels have no pid (#426).** `KernelSession::shutdown` only
  kills processes with a pid, so a gateway kernel got a
  `shutdown_request` and no `DELETE`. Against Ganymede as the remote that
  looked fine (it treats the requested exit as final); `jupyter_server`
  restarts the kernel. `KernelSession::shutdown` now calls `kill` on a
  launched kernel with no pid (an in-process kernel answers Unsupported
  and is left alone), so the `DELETE` is sent and awaited. Dropping the
  last `RemoteKernel` sends it too, as a fallback for a restart or a
  dropped websocket; that one needs a runtime and is best effort.
