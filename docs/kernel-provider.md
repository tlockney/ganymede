# Kernel providers

A design note for the kernel provider channel in
[#965](https://github.com/tlockney/ganymede/issues/965), section 3: a
local, token-protected way for a client of `ganymede serve` to register a
kernelspec and run its kernels itself. It is new network surface, so this
note comes before any code.

**Status: proposed.** Nothing here is built yet.

## The problem

The Mac app runs a TypeScript kernel in a web view. Since ganymede-ui
0.7.0 it registers that kernel with the server it embeds, through the C
ABI in [In-app kernels as server kernels](in-app-kernel.md): the server
lists it as a kernelspec, starts and stops it through the app's
callbacks, and exchanges Jupyter messages with it, so rooms, the execute
endpoint, agents and the kernel list all treat it as one more kernel.

#965 moves the Mac app toward one engine per Mac: a background
`ganymede serve` that keeps running with no window open, which the app
opens local notebooks through. That server is another process. The C ABI
can't reach it, so the in-app kernel would vanish from every notebook
opened through it. Until this channel exists, the in-app kernel stays on
the app's embedded server.

## The proposal

A websocket on the server, `/api/kernel-providers/connect`, carries the C
ABI's calls as JSON frames, one to one. The app, or any other local
client, connects, registers one or more kernelspecs, and answers the
server's start, send and stop for their kernels. On the server, each
connection becomes an `AppKernel` behind the existing
`RegisteredKernels` (`crates/ganymede-kernel-host/src/registered.rs`), so
nothing past the socket changes: the kernels run through the same
sessions, rooms, execute endpoint, restart policy and loss handling as
the embedded app's kernel does today.

### Frames

Text frames, each one JSON object with an `op`. Jupyter messages are the
websocket's JSON form (`header`, `parent_header`, `metadata`, `content`,
`buffers` as base64), as the C ABI carries them.

From the provider:

| `op` | Fields | C ABI equivalent |
| --- | --- | --- |
| `register` | `spec` (kernel.json's keys plus `name`) | `ganymede_kernel_register` |
| `unregister` | `name` | none (new) |
| `started` | `kernel_id`, or `kernel_id` and `error` | `start`'s return value |
| `deliver` | `kernel_id`, `channel`, `msg` | `ganymede_kernel_deliver` |
| `ended` | `kernel_id`, `reason` (`exited` or `lost`) | `ganymede_kernel_ended` |

From the server:

| `op` | Fields | C ABI equivalent |
| --- | --- | --- |
| `registered` | `name`, or `name` and `error` | `register`'s return value |
| `start` | `kernel_id`, `launch` (`kernelspec`, `cwd`, `env`) | `start` |
| `send` | `kernel_id`, `channel`, `msg` | `send` |
| `stop` | `kernel_id` | `stop` |

`started` exists because a socket call can't return a value. The server
waits for it before waiting for `kernel_info_reply`, within the same
startup timeout.

### Who may connect

A registered kernel sees every cell any client runs on it: an agent over
MCP, another notebook, a run. So registering is more than running code,
and the route is narrower than the rest of the API:

- **Off by default.** `[kernel_providers] enabled = true` in the server's
  config turns it on. The background server the Mac app starts turns it
  on; a `ganymede serve` started by hand doesn't.
- **The server's token only.** A cookie or a tailnet login is refused
  with 403, so a page in a browser, a guest in a room and a tailnet peer
  can't register, even when they can use the rest of the API.
- **Loopback peers only.** The connection's peer address must be
  `127.0.0.1` or `::1`, whatever `ip` the server listens on. A request
  `tailscale serve` forwards, which arrives from loopback, carries
  Tailscale's identity headers, and is refused too.
- **Not through a machine mount.** The mount's list of forwarded routes
  (`api/kernels`, `api/events/subscribe`, …) doesn't gain this one.
- **No shadowing.** A name that is already a kernelspec, on disk, from a
  kernel server, or registered by another connection, is refused. A
  provider can't take over `python3` and see cells meant for it. (The C
  ABI's registration goes in place of a kernel server's spec of the same
  name, because the embedding app is trusted; a socket's isn't.)

Within those limits, a provider is as trusted as anything else holding
the token, which can already start kernels and read every file. The
token lives in the server's `jpserver-<pid>.json`, which only this user's
processes can read.

### Lifetime

- **A registration lasts as long as its connection.** When the socket
  closes, its kernelspecs are unregistered and each of its running
  kernels ends as `lost` (`RegisteredKernels::ended`). The restart policy
  then tries to restart it, and that fails while the spec is gone, so the
  kernel ends `dead` with `"reason": "lost"`. A provider that reconnects
  registers again and gets new kernels. `RegisteredKernels` gains
  `unregister(name)` for this.
- **Ordering** is the C ABI's: per kernel, messages keep their order in
  both directions.
- **Backpressure.** The server's frames to a provider go through a
  bounded queue. A provider that takes no frame for 10 seconds is
  disconnected, as an events-socket client that stops reading is
  ([Hear of a write](api.md#hear-of-a-write)), and its kernels are lost.
- **Server restart** closes every provider's socket. The app reconnects
  when the server comes back, as it reconnects its other sockets.

### What changes in each repository

- **ganymede**: the route, config key and `AppKernel` over a socket in
  ganymede-server; `RegisteredKernels::unregister` and the no-shadowing
  check in ganymede-kernel-host; docs/api.md and docs/security.md. Tests
  drive a fake provider over a real socket: register, start, a message
  each way, interrupt, shutdown, a dropped connection, each refusal
  above, and a slow provider.
- **ganymede-ui**: when the app's notebooks go through the background
  server (#965 section 4), `KernelHost` registers the TypeScript kernel
  there over the socket instead of through the C ABI. The kernel page and
  cdp-kernel don't change.

## Alternatives considered

- **A kernel server the app runs** (`gateway_url`). The background server
  would forward kernels to an HTTP kernel server inside the app. That
  needs the app to serve the whole kernels API and listen on a port of
  its own, which is more surface, not less.
- **A Unix socket in the runtime directory** instead of a route. It would
  make "this user, this machine" a property of the file's mode rather
  than of a peer-address check. It adds a second listener and a second
  transport to the server and to every client, and the Mac app's sandbox
  rules for Unix sockets differ from TCP's. The token plus loopback check
  gives the same boundary on the existing listener. If the loopback check
  proves hard to keep correct behind proxies, this is the fallback.
- **No channel**: keep the in-app kernel on the embedded server only. The
  one-engine plan then can't route local notebooks through the
  background server without losing the in-app kernel from them.
