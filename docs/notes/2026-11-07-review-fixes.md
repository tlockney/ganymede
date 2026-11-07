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
  `--checkpoint-dir`.
- **Allocation failure is an abort, not a panic (#427).** A v3 heading's
  `level` went straight into `"#".repeat(level)`. Python raises
  `MemoryError`; Rust aborts the process, and `spawn_blocking` can't
  catch that. Clamped at 1024.
- **Stack overflow is an abort too (#428).** yrs 0.28's `Any::decode`
  recurses per level with no limit, at two bytes a level, and a 1 GiB
  message limit let a client nest as deep as it liked. There is no yrs
  option to bound it, so `depth.rs` walks a v1 update iteratively, along
  `Update::decode`, `ItemContent::decode` and `Any::decode`, and the room
  refuses one deeper than 256. A malformed update passes the scan and
  yrs reports it. The same overflow came from the other side: shared maps
  nested 20,000 deep (300 KB, each level its own item, so the scan can't
  see it) overflowed `out_to_json` when the room built the notebook.
  That now stops at the same depth. JSON strings yrs parses need nothing:
  `serde_json` stops at 128 levels.
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
  restarts the kernel. Dropping the last `RemoteKernel` now sends the
  `DELETE`, which covers shutdown, restart and a dropped websocket.
