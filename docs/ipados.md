# The iPad use case

The case that shaped Ganymede: TypeScript notebooks on an iPad, with no
Python anywhere. It started as the
[ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados)
design: an iPad client in front of kernels on a home server over a
tailnet, then a kernel on the iPad itself. This page says what that
asked of Ganymede, what was built, and what is left. Using the app is
covered in [Ganymede's app](app.md).

## What was built

- **The app.** One SwiftUI app for the Mac and iPad, in ganymede-ui's
  [`apple/`](https://github.com/tlockney/ganymede-ui/tree/main/apple),
  runs Ganymede in its own process through `ganymede-ffi` and shows
  ganymede-ui in a web view. The plan had the web view reach the core
  through a message bridge with no HTTP; running the ordinary server on
  loopback inside the app was simpler and needed no second API.
- **Kernels on the iPad.** iOS doesn't let an app launch processes, so
  the page runs cdp-kernel's TypeScript kernel in a Worker in the web
  view. A native kernel in the app's process is next.
- **Remote kernels.** With a kernel server set, Ganymede in the app sends
  kernels to it through the gateway, as `jupyter_server --gateway-url`
  does, and notebooks stay on the iPad. The kernel server is any Jupyter
  or Ganymede server, such as `ganymede serve` on a Mac.
- **Kernels outlive the connection.** iPadOS suspends apps and drops their
  websockets. A kernel keeps running, and a client that reconnects with
  the same session gets the output it missed. Idle culling
  (`--cull-idle-secs`) stops abandoned kernels piling up.
- **Files as the source of truth.** Notebooks can live in iCloud Drive or
  on a share, written underneath the server by other devices. Writes are
  atomic (temp file, then rename), so a sync never sees half a notebook;
  checkpoints can go outside the synced folder (`--checkpoint-dir`); and
  a save can be made conditional on the file not having changed (`ETag`
  and `If-Match`, or `last_modified`).

![Where each piece runs in four setups: on a workstation, the frontend, ganymede serve and each kernel are separate processes; in the Mac app, ganymede-ui and Ganymede run inside the app and kernels are the Mac's processes; in the iPad app everything runs inside the app and the kernel runs in the page; in a browser with no server, the UI, kernels and IndexedDB storage all stay in one tab.](images/architecture/where-kernels-run.svg)

## What is left

- **A native on-device kernel**: cdp-kernel in the app's process, behind
  the `Provisioner` and `TransportFactory` seams that already accept a
  kernel with no ZeroMQ.
- **Reloading notebooks changed on disk** (the document model,
  [#141](https://github.com/tlockney/ganymede/issues/141)). Today a
  change from another device shows on the next open, and a save that
  sends the file's `ETag` or `last_modified` is refused rather than
  overwriting it.
- **`tailscale serve` identity**: trusting the identity headers it adds,
  so a tailnet client needs no token.
- **Distribution**: the app is built from source or taken from CI, not
  signed for the App Store.

## Decisions that came out of it

- **Remote clients speak the REST API and websocket, never ZeroMQ.** That
  makes `jupyter_server`'s API the one contract every client depends on,
  which is why it is a compatibility target, not a design space.
- **The file is the truth; a CRDT is optional and later.** With iCloud
  there are two writers of one file: the server and the Files app. A
  server-side CRDT would be authoritative only for clients connected to
  it, and the file would still have to be re-read when it changes. For
  one person on a few devices, last writer wins with a conditional save
  is enough.
- **Kernel metadata passes through untouched.** cdp-kernel writes
  `metadata` into `kernel.json` and extra fields into `kernel_info_reply`
  (JupyterLab reads `metadata.debugger`, and the plan adds
  `metadata.capabilities`). The host has no schema that would strip them,
  and the conformance tests check it.
