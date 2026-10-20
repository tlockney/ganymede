## 2026-10-20, binary buffers through the websocket, contents without SQLite (#68)

The last two items PR #110 left open:

- `websocket_carries_binary_buffers_both_ways` (`ganymede-server/tests/http.rs`) opens a comm on the fake kernel's `echo` target and sends a `comm_msg` with four buffers (`<IDS|MSG>`, empty, all 256 byte values, 100 KB) over a v1-subprotocol websocket and again over a legacy one (binary frames out, binary frames back). The echo comes back byte for byte both ways. With #110's `zmq_kernel.rs` (the same buffers through ZMQ framing) and the real-kernel comm echoes in `kernels.rs`, every hop a buffer takes now has a test.
- `cargo test --workspace` unifies features, and the server turns on `ganymede-contents/sqlite`, so the in-memory signature store had only ever been compiled. CI's `rust` job now also runs `cargo test -p ganymede-contents`, which builds the crate alone without the feature; `trust_survives_a_restart_with_the_same_data_dir` already had its no-SQLite expectation waiting.

The `lab install` hostile-wheel item was already covered by unit tests in `ganymede-cli/src/lab/` (hash mismatch, yanked, host allowlist checked on the final URL after redirects, hostile extension names, escaping entries, symlinks, size caps), so nothing was added there.

Lesson: a feature that a workspace member always enables is never off under `--workspace`; test the crate on its own to cover the default.
