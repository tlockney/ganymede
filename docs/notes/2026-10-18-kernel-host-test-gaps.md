## 2026-10-18, fake-kernel modes and real-process tests for the kernel host (#68)

The fake kernel spoke only what the first session tests needed. It now
also covers kernel output and lifecycle that real kernels produce:
`exit` prints, then vanishes mid-cell. `display` sends display, update
and clear. Comms work: target `echo` sends each message back with its
buffers, any other target is closed as ipykernel closes it,
`comm_info_request` lists the open comms, and `comm` opens one from the
kernel side. After an error under `stop_on_error`, queued
`execute_request`s are answered `aborted`. Session tests use each of
these.

`ganymede-kernel-host` gained a `tests/` directory:

- `subprocess.rs` launches shell scripts as kernels. Each one checks
  something the host does to a real process:
  - the cwd, kernelspec and launch env, `JPY_SESSION_NAME` and
    `JPY_PARENT_PID`, and a connection file that the kernel reads and
    that is removed with the kernel
  - SIGINT reaching both the kernel and the child it waits on
  - a shutdown that escalates to SIGTERM, then SIGKILL, and one that
    stops at SIGTERM
  - an exit noticed on its own
- `zmq_kernel.rs` binds ROUTER, PUB and REP sockets with a signing key,
  then drives the startup nudge and the heartbeat against them. It sends
  binary buffers through the real framing: one equal to `<IDS|MSG>`, an
  empty one, all 256 byte values and 100 KB. They come back unchanged. A
  wrong key never completes a nudge.

Already covered elsewhere: the ws decoders' malformed-input tests (#48)
and the server config tests (#55). Not covered here: binary buffers
through the websocket layer, which belongs with the server tests.

Learned: a non-interactive `sh` starts `&` jobs with SIGINT ignored, and
a trap cannot undo that, so a group-interrupt test needs a foreground
child.
