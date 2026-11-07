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
    possibly its own update. yrs deletes them recursively
    (`TransactionMut::delete`), so a tree that is in the document can
    never be taken out again, and an update that builds one and deletes
    it (or overwrites its key) aborts while it is being applied. A first
    try applied type-adding updates to a scratch copy and measured that;
    the second review crashed the copy that way, and the copy cost two
    whole-document encodes per added cell. Now the scan records each
    item's clock range and parent (a named root, the item holding the
    parent type, or an origin whose parent it shares), and the depth of
    each new shared type is worked out from that, without applying
    anything. Parents already in the document come from a map of branch
    depths by item id, measured when the room loads and again when an
    update names a branch the map lacks (the server's own edits add
    some); a sibling that isn't a shared type has no depth of its own, so
    the document's deepest stands in. Past 16 levels the update is
    refused, as is one that hangs shared types on items the room doesn't
    have: yrs would hold it pending and integrate it later, at a depth
    nobody checked. The notebook schema goes 5 deep.
  - 100 + 16, plus the notebook's own levels, stays under the 128 levels
    `serde_json` reads back, so a room can't save a file Ganymede can't
    open. `out_to_json` stops one level past 16 as a backstop (values
    inside the deepest accepted type are kept) and gives null for XML
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
  and is left alone), so the `DELETE` is sent and awaited, within the
  shutdown's grace: the gateway client has only a connect timeout.
  Dropping the last `RemoteKernel` sends it too, as a fallback for a
  restart, a dropped websocket or a `DELETE` that failed (a failed one
  leaves the kernel marked alive); that one needs a runtime and is best
  effort.
