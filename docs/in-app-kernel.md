# In-app kernels as server kernels

A design note for [#266](https://github.com/tlockney/ganymede/issues/266):
registering the kernel the Apple apps run in-app with the server embedded
in the same app, so it becomes a kernel like any other.

The apps' part shipped in ganymede-ui 0.7.0
([ganymede-ui#470](https://github.com/tlockney/ganymede-ui/pull/470)).
The next section describes the apps before it.

## The problem

On the iPad and iPhone, a cell never reaches Ganymede. The notebook page
runs its own kernel service (`@jupyterlite/services`, the `kernels:
'page'` backend), `KernelHost.swift` relays Jupyter messages to a hidden
second web view (`kernel.html`), and cdp-kernel's browser kernel runs the
cell in a Worker there. The server inside the app only stores files. The
Mac app runs the same page kernel beside its subprocess kernels
(`kernels: 'both'`), with the same gap.

So nothing the server adds reaches a notebook running on the in-app
kernel:

- The [execute endpoint](rooms.md#running-cells-into-a-room), which writes
  outputs into a document room, and the agent tools built on it.
- Kernel lifetime: a kernel ends with the page that started it, and each
  window has its own.
- The kernel list, idle culling, kernel logs, and later the run contract
  (named parameters and results) and the run ledger.

![Two rows. Today: the notebook page runs a kernel service of its own and relays messages through KernelHost.swift to the kernel page, where cdp-kernel's browser kernel runs cells in a Worker; the embedded Ganymede server beside them only serves files. With registration: the notebook page and other clients use the server's ordinary HTTP and websocket API; the server's sessions, kernels and rooms reach the registered kernel through the C ABI, and KernelHost.swift passes its messages to the same kernel page and Worker.](images/in-app-kernel/registration.svg)

## The proposal

The app registers its kernel with the embedded server through the C ABI.
The server lists it as a kernelspec, starts and stops it through the
app's callbacks, and exchanges Jupyter messages with it per channel. To
the rest of the server it is a kernel with no process and no ZeroMQ,
which the `Provisioner` and `TransportFactory` seams already accept
([Embedding](embedding.md#without-zeromq-or-subprocesses)).

The first step keeps today's kernel. The kernel page, the Worker and
cdp-kernel's browser build stay as they are. What changes is what they
talk to: Swift connects the kernel page to the Rust server instead of to
the notebook page, and the notebook page uses the ordinary HTTP and
websocket API, as it does against `ganymede serve`. The engine (a native
V8 or QuickJS kernel) is a later, separate decision that this
registration does not depend on.

### The C ABI

Three functions and one callback table join `include/ganymede.h`:

```c
typedef struct GanymedeKernelCallbacks {
    void *ctx;
    /* Start kernel `kernel_id`. launch_json: {"kernelspec", "cwd", "env"}.
     * Return 0, or nonzero to fail the start. */
    int (*start)(void *ctx, const char *kernel_id, const char *launch_json);
    /* A Jupyter message for the kernel, as JSON, on "shell", "control" or
     * "stdin". */
    void (*send)(void *ctx, const char *kernel_id, const char *channel,
                 const char *msg_json);
    /* End the kernel now. The server has already sent shutdown_request
     * and waited, as for any kernel. */
    void (*stop)(void *ctx, const char *kernel_id);
    /* No callback follows this one: ctx may be freed. */
    void (*release)(void *ctx);
} GanymedeKernelCallbacks;

/* Lists spec_json (kernel.json's keys, plus "name") as a kernelspec whose
 * kernels these callbacks run. 0 on success. */
int ganymede_kernel_register(GanymedeServer *server, const char *spec_json,
                             const GanymedeKernelCallbacks *callbacks,
                             char **error_out);

/* A message from the kernel, on "shell", "control", "stdin" or "iopub". */
int ganymede_kernel_deliver(GanymedeServer *server, const char *kernel_id,
                            const char *channel, const char *msg_json);

/* The kernel ended without being asked to. reason: "exited" or "lost". */
void ganymede_kernel_ended(GanymedeServer *server, const char *kernel_id,
                           const char *reason);
```

The contract:

- **Messages are Jupyter JSON per channel.** The shape is the websocket's
  JSON protocol (`header`, `parent_header`, `metadata`, `content`), with
  binary buffers as base64 strings in `buffers`, as today's relay
  carries them. cdp-kernel's browser build already speaks this through
  `PageChannels` (`deliver(channel, msg)` and `onMessage`), so the kernel
  needs no change. Nothing is signed: both ends are one process.
- **Threads.** Callbacks run on a server thread, never the app's main
  thread, and must not block. Swift hops to the main actor to reach the
  web view. `ganymede_kernel_deliver` and `ganymede_kernel_ended` take
  any thread and don't block. Messages for one kernel keep their order in
  both directions.
- **Lifetime.** Strings passed to a callback are valid during the call.
  `ctx` stays the app's until `release`, which the server calls when it
  stops. When `ganymede_kernel_register` fails, it never calls `release`
  and keeps no pointer, so `ctx` stays the app's to free. Registering a
  name that is already registered fails, and the first registration
  stays. Register after `ganymede_server_start` and before the web view
  loads the page, so the first kernel list includes the kernel.
- **Start.** `start` returns at once. The server then waits for
  `kernel_info_reply` within its startup timeout, as for a subprocess
  kernel. `launch_json` carries the notebook's path in
  `env.JPY_SESSION_NAME` (#627), which the app's permission ledger needs.
- **Interrupt and stop.** The kernelspec says `"interrupt_mode":
  "message"`, so an interrupt is an `interrupt_request` on control, which
  cdp-kernel's browser kernel already handles. Shutdown sends
  `shutdown_request` first, then calls `stop` after the grace period.
- **Heartbeat.** A registered kernel is alive until `ganymede_kernel_ended`
  or `stop`.
- **Names.** A registered kernelspec's name follows the kernelspec rules
  (lowercased, `[a-z0-9._-]`). It is listed after a kernel server's
  (`gateway_url`), in place of one of the same name, so the server's
  default kernel stays the default unless it has no kernels of its own.

On the Rust side, a `RegisteredKernels` type implements
`KernelspecSource`, `Provisioner` and `TransportFactory` over the
callbacks, and `ganymede-ffi` puts it in front of the gateway's kernels,
or of none. A test drives a fake kernel through the C functions, and
`tests/smoke.c` registers one.

### What changes in each repository

- **ganymede**: the functions above, `RegisteredKernels`, the header, and
  [Embedding](embedding.md#the-server-in-an-app-from-c). This closes #266.
- **ganymede-ui, Swift**: `KernelHost` registers the TypeScript kernel at
  server start and maps the callbacks onto the kernel page's existing
  `start`, `send`, `message` and `closed` operations. The kernel page
  belongs to the app rather than to a window, since kernels now outlive
  windows and reloads. The ledger takes a kernel's notebook from
  `launch_json`; `moved` and `declared` stay as they are.
- **ganymede-ui, web**: in the apps, the notebook page connects with the
  `ganymede` backend and lists the server's kernels, the in-app one among
  them. `kernel-relay.ts` loses its notebook-page half. In a browser with
  no server, page kernels work as today.
- **cdp-kernel**: none expected.

### When the kernel is lost

iOS ends a suspended app's kernel after about 30 seconds, and ends the
kernel page's process under memory pressure. Today the notebook page
shows the kernel as dead with Restart, and state and unsaved outputs are
gone.

With registration, Swift calls `ganymede_kernel_ended(server, id,
"lost")` for each kernel the page held. The server treats it as a crash
with two differences:

- It restarts the kernel, unless restarts are off (`restart_limit` 0),
  without counting toward `restart_limit`, since the platform ended it,
  not a fault in the kernel. With no count, a kernel the platform keeps
  ending restarts each time; see Restart after a loss below. A loss
  before the kernel's first `kernel_info_reply` is a failed start: the
  server launches it up to three times in all, as for any kernel.
- The `status: restarting` message on iopub carries `"reason": "lost"` in
  its content. Clients that don't know the key ignore it, as Jupyter
  clients ignore unknown content keys.

The app can then show "iOS stopped the kernel while the app was in the
background" with a **Restore** action. Restore re-runs, on the new
kernel, the cells that had run, in the order of their execution counts,
and stops at the first error. It runs each cell's current source, so a
cell edited after it ran runs as edited; the action lists the cells
before it runs them. Outputs survive the loss because they live in the
notebook (and, once ganymede-ui opens notebooks through rooms, in the
server's document), not in the kernel.

The first version of Restore is a ganymede-ui action over the ordinary
kernel client, or over the execute endpoint once the app uses rooms.
When the run contract lands, it becomes "run this notebook's executed
cells", a server operation any client can call.

`"exited"` is a kernel that ended for its own reasons, such as a Worker
that failed to load. It follows the ordinary crash path and counts
toward the limit.

### Capabilities

Kernels differ in what cells can use: real files, `node:` modules, the
`device` API, the debugger. The in-app kernel lacks most of what a Mac
subprocess kernel has, so the picker and errors should say so.

- **Before a kernel starts**, the kernelspec says what it offers, in
  `metadata.capabilities`, an object keyed by name (`fs`, `node`,
  `device`, `debugger`, `stdin`), the same shape as
  `kernel_info_reply.capabilities` below, so a client reads both one way.
  A value is `true`, or something more specific, such as the methods of a
  namespace. The host passes kernelspec metadata through
  untouched, so this needs no server change. The kernel picker shows them
  as badges, and lists in-app and remote kernels together.
- **Once it runs**, `kernel_info_reply.capabilities` says what it got.
  cdp-kernel already sets `capabilities: { device: true }` there when the
  app gives the kernel page a `device` bridge.
- **Later**, a cell error caused by a missing capability names it, so the
  page can offer to run the cell on a kernel that has it. That needs an
  error convention in cdp-kernel and is not part of the first step.

### Where the kernel core runs with a native engine

The first step keeps the Worker. When a native engine replaces it, the
kernel's own code (message handling, TypeScript stripping, completion,
later a bundler) can run in one of two places. Both use the same
registration, so this choice doesn't change the C ABI.

![Two options for a native engine. In the first, the kernel core stays in the kernel web view, where JavaScriptCore has its JIT; BrowserTarget reaches the native engine through its three-method WorkerLike interface, bridged by Swift to the engine's thread, which runs only cells. In the second, the kernel core runs beside the engine in the app's process and drives cells over the inspector protocol, with no web view; it brings the debugger and runs where no web view can, at the cost of more new code and running the core without a JIT. Both register with the embedded server through the same C ABI.](images/in-app-kernel/kernel-core.svg)

- **In the web view, behind `WorkerLike`.** `BrowserTarget` reaches its
  Worker through three methods (`postMessage`, `addEventListener`,
  `terminate`). A Swift object with those methods can stand in front of
  the native engine, and the kernel core stays in the web view, where
  JavaScriptCore has its JIT. This is the least new code. It needs a web
  view, and it has no debugger, since `BrowserTarget` drives cells with
  its own messages rather than the inspector.
- **Beside the engine, over the inspector.** The kernel core runs in the
  app's process with the engine and drives cells through the inspector
  protocol, as cdp-kernel's CDP targets do. The debugger comes with it,
  and it runs where no web view can: a background task, an extension.
  It costs more new code, and the core runs without a JIT.

I would start in the web view and move the core beside the engine when
the debugger or runs without a web view are wanted. Either way, a run
with no web view needs scripts stripped and bundled when they are saved.
Which engine (rusty_v8 without JIT, or QuickJS) stays open, to be
decided by measuring a build.

## Why the C ABI

The C ABI grows by three functions, a callback table, and their `unsafe`
glue, in the patterns `ganymede-ffi` already uses. Nothing existing
changes. It adds no network surface and no dependency.

The alternative was a websocket endpoint where the kernel page dials into
the server as a kernel. It needs no C change and would also work in a
browser tab. It was set aside because:

- It opens a new network endpoint that receives every cell sent to a
  kernel, to be protected beyond the token.
- The server still can't start the kernel page, so `start` and `stop`
  would need the C ABI anyway, and the kernel would be split across two
  channels.
- A native engine has no page to dial from. The C ABI serves today's
  kernel, a native one, Pyodide and a later SQL kernel alike.

The cost of going through Swift is one more hop per message, which
today's relay already pays.

## Open questions

- **Restart after a loss.** The proposal restarts at once, so the
  notebook has a kernel when the app comes back. The alternative leaves
  it dead until the person picks Restart or Restore, which starts no
  kernel the person didn't ask for. Losses don't count toward
  `restart_limit`, so nothing yet caps or spaces out restarts after
  repeated losses, such as a page killed under memory pressure each time
  it starts.
- **Where the kernel web view lives on the Mac.** WebKit suspends a web
  view in no window. With kernels owned by the app, the kernel web view
  needs a home that outlives any one window.
- **Synchronous requests from page kernels.** The TypeScript kernel
  doesn't use the service-worker relay, but JupyterLite's kernels, the
  planned Pyodide one among them, read files and ask for input through it,
  answered by the notebook page's kernel service. Once that service is
  gone, Pyodide would read files through the server's contents API and
  ask for input over the stdin channel. This matters when Pyodide
  registers, not for the first step.

## What this leaves out

The engine choice, Pyodide's order, Shortcuts and widgets on the kernel,
pairing with a Mac, and the kernel picker's design. Each can build on the
registration once it exists.
