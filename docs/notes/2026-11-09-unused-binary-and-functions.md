## 2026-11-09, remove the ganymede-server binary and four unused public functions (#630)

Items 4 and 5 of the project read of 2026-11-09 (#629).

- **The `ganymede-server` binary is gone.** `crates/ganymede-server` had
  a `main.rs` that parsed `ServeArgs` and called `serve::run`: what
  `ganymede serve` does, without the CLI's other flags. No release,
  image or script shipped it, and only the crate's README named it, but
  every workspace build and test run linked it. The crate is a library
  now; its README shows `cargo run -p ganymede-cli -- serve`.
- **Four public items had no caller** in this repository, ganymede-ui,
  cdp-kernel or ganymede-device, and no page of the docs names them:
  `ContentsModel::without_content`, `iopub_topic`, `host_socket_kind`
  with its `SocketKind`, and `Cell::metadata_mut`. The ZeroMQ transport
  (`ganymede-kernel-host/src/zmq.rs`) subscribes to every iopub topic
  and connects each socket by its type itself, so it needs neither
  `iopub_topic` nor `host_socket_kind`.

This is an API change for an embedder: the workspace is
`publish = false`, but `ganymede-engine` re-exports `ganymede-nbformat`
and `ganymede-protocol`, so three of the four were reachable as
`ganymede_engine::nbformat::Cell::metadata_mut` and
`ganymede_engine::protocol::wire::{iopub_topic, host_socket_kind}`. A
`Cell`'s variants and their `metadata` fields are public, so a caller
that wants to change metadata matches on the cell, as the code here
does. Left in place on purpose: `to_writer` (the nbformat README names
it), `Engine::adopt_launched` (Embedding documents it) and
`ExecuteBody.client_id` (accepted and ignored, as jupyter_server's
clients send it).

How they were found: for each `pub fn` and `pub enum` in the workspace,
a search for its name outside its own definition, then a look at each
hit.
