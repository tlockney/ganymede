# The iPad use case

The case that shaped Ganymede: TypeScript notebooks on an iPad, with no
Python anywhere. It started as the
[ts-on-ipados](https://github.com/tlockney/research/tree/main/ts-on-ipados)
design: an iPad client in front of kernels on a home server over a
tailnet, then a kernel on the iPad itself. This design note says what
that asked of Ganymede, what was built, and what it decided. Using the
app is covered in [Ganymede's app](app.md); what is still planned is on
the [Roadmap](roadmap.md).

## What was built

- **The app.** One SwiftUI app for the Mac and iPad, in ganymede-ui's
  [`apple/`](https://github.com/tlockney/ganymede-ui/tree/main/apple),
  runs Ganymede in its own process through `ganymede-ffi` and shows
  ganymede-ui in a web view. The plan had the web view reach the core
  through a message bridge with no HTTP; running the ordinary server on
  loopback inside the app was simpler and needed no second API.
- **Kernels on the iPad.** iOS doesn't let an app launch processes, so
  the app runs cdp-kernel's TypeScript kernel in a Worker in a hidden
  second web view, and relays its messages to the notebook's page.
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
- **Reloading notebooks changed on disk.** A notebook opened through a
  document room is reloaded when another device changes the file, and
  edits on both sides are merged cell by cell
  ([Live editing and document rooms](rooms.md)).
- **Distribution.** The Mac app is signed and notarized in each
  ganymede-ui release, and the iPad and iPhone app goes to TestFlight.

![Where each piece runs in four setups: on a workstation, the frontend, ganymede serve and each kernel are separate processes; in the Mac app, ganymede-ui and Ganymede run inside the app and kernels are the Mac's processes; in the iPad app everything runs inside the app and the kernel runs in the page; in a browser with no server, the UI, kernels and IndexedDB storage all stay in one tab.](images/architecture/where-kernels-run.svg)

## Decisions that came out of it

- **Remote clients speak the REST API and websocket, never ZeroMQ.** That
  makes `jupyter_server`'s API the one contract every client depends on,
  which is why it is a compatibility target, not a design space.
- **The file is the truth.** With iCloud there are two writers of one
  file: the server and the Files app. A server-side CRDT is authoritative
  only for clients connected to it, so the file still has to be re-read
  when it changes. Document rooms keep a `yrs` document for live editing
  but load from the file, save to it, and merge in changes made on disk.
- **Kernel metadata passes through untouched.** cdp-kernel writes
  `metadata` into `kernel.json` and extra fields into `kernel_info_reply`
  (JupyterLab reads `metadata.debugger`, and the plan adds
  `metadata.capabilities`). The host has no schema that would strip them,
  and the conformance tests check it.
