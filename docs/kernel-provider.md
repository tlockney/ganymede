# Kernel providers

A design note for the kernel provider channel in
[#965](https://github.com/tlockney/ganymede/issues/965), section 3: a
local way for a program of the same user to register a kernelspec with a
running `ganymede serve` and run its kernels itself. It is new surface
on the server, so this note comes before any code.

**Status: implemented (#983), the reservation of a name for one program
(#986) included.**

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release. The latest release doesn't know
> `[kernel_providers]` and refuses to start with it in its config.

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

The server listens on a Unix domain socket in the Jupyter runtime
directory, `ganymede-kernels-<pid>.sock`, and names it in its
`jpserver-<pid>.json` as `kernel_providers_socket`. A provider connects,
registers one or more kernelspecs, and answers the server's start, send
and stop for their kernels. The frames carry the C ABI's calls one to
one. On the server, each connection is an `AppKernel` behind the
existing `RegisteredKernels`
(`crates/ganymede-kernel-host/src/registered.rs`), so the kernels run
through the same sessions, rooms, execute endpoint and kernel list as the
embedded app's kernel does today. The changes that takes are listed below.

### Why a Unix socket, not a route

A registered kernel sees every cell any client runs on it, from an agent
over MCP, another notebook or a run, and its replies become the outputs.
So who may register matters more than who may call the API, and an HTTP
route can't say it well:

- "Loopback only" doesn't exclude a proxy. `tailscale serve` sends no
  identity for tagged devices or `tailscale funnel` traffic, and `ssh -L`
  or any local reverse proxy also arrives from loopback with no header to
  refuse.
- The API takes the token in the query string, so a browser page opened
  from a `?token=` URL holds it.
- A machine mount, a gateway or a later feature could forward the route by
  mistake.

A socket file in the runtime directory makes the boundary the file
system's: the directory is `0700` and the socket `0600`, so only this
user's processes can connect, and nothing on the network, behind a
proxy or in a browser can. It isn't a route, so nothing forwards it.
The server makes the directory `0700` if it is missing, then checks it
has no group or other write bit and is owned by the server's own user;
anything else and the socket isn't opened: in a directory another user
can write, the socket's own mode would mean nothing. The socket is made
in a private temp dir inside the runtime dir, made `0600` there, and
only then renamed into place, so it is never visible with a wider mode.
A path over the platform's `sun_path` limit (103 bytes on macOS and the
BSDs, 107 on Linux and Android) means no socket, with a warning naming
the path and its length; the staged path is checked too, as it can be
the longer of the two.

### Who may connect

- **Off by default.** `[kernel_providers] enabled = true` in the server's
  config makes the server open the socket. The background server the Mac
  app starts turns it on; a `ganymede serve` started by hand doesn't. With
  auth off (`token = ""`) the socket stays closed: anyone who can reach
  the port can run code already, so a provider check adds nothing.
- **This user only**, by the file modes above. The server also checks the
  peer's user id (`SO_PEERCRED` on Linux, `getpeereid` on macOS) and
  closes a connection from any other user.
- **The token as well.** The first frame is `hello` with the server's
  token, so a process that can connect but never read the server's info
  file (a sandboxed helper given the path) still can't register.
- **Unix only at first.** On Windows the server doesn't open the socket;
  a named pipe can follow if someone needs it.

A process that passes all of this runs as the same user as the server.
It can already read the token, every file and every notebook, and start
kernels. This user's processes are inside the boundary; the channel
doesn't widen it, and the rules below keep a provider from reaching
further than its own kernels.

### What a provider may register

- **Only display fields.** A spec from a provider gives `name`,
  `display_name`, `language` and `metadata`. The server drops `argv`,
  `env` and anything else, so no spec from the socket ever names a
  program to run.
- **No name that exists elsewhere.** A name that is a kernelspec on disk
  or on a kernel server is refused at registration. A provider's specs
  are also listed and launched after those, so one installed later wins
  over the provider's, never the other way round. (The C ABI's
  registration goes in place of any spec of the same name, on disk or on
  a kernel server, because the embedding app is the server's own
  process.)
- **Names are first come, unless reserved.** While a connection holds a
  name, another can't register it. A name can be reserved for one program
  with `[kernel_providers] reserve = { "cdp-browser" = "<code requirement>" }`:
  the requirement is a `csreq` string, as `codesign -r` takes it, such as
  `identifier "net.lockney.Ganymede" and anchor apple generic and
  certificate leaf[subject.OU] = "ABCDE12345"`. On a connection, the
  server reads the peer's audit token from the socket
  (`LOCAL_PEERTOKEN`, not the pid, which another process can reuse) and
  checks the peer's code signature against the requirement with
  `SecCodeCheckValidity`. A process that connects before the app can't
  take the app's name, whether or not the name is free. The name is
  refused with `kernelspec <name> is reserved for another program` when
  the requirement fails, and just as much when the peer's signature can't
  be read or the requirement doesn't parse (the server logs a bad
  requirement once at startup); off macOS, where no signature is read,
  every reserved name is refused to everyone.
- **Capabilities are the provider's claim.** `metadata.capabilities`
  (`device` and the others in
  [Capabilities](in-app-kernel.md#capabilities)) is passed through as the
  provider gives it, as it is for the C ABI; a provider is a process of
  this user, and the reservation above is what keeps another process from
  claiming the app's kernel.

### Frames

Newline-delimited JSON, one object per line with an `op`. Jupyter
messages are the websocket's JSON form (`header`, `parent_header`,
`metadata`, `content`, `buffers` as base64), as the C ABI carries them.

From the provider:

| `op` | Fields | C ABI equivalent |
| --- | --- | --- |
| `hello` | `token` | none |
| `register` | `spec` (the fields above, plus `name`) | `ganymede_kernel_register` |
| `unregister` | `name` | none (new) |
| `started` | `kernel_id`, or `kernel_id` and `error` | `start`'s return value |
| `deliver` | `kernel_id`, `channel`, `msg` | `ganymede_kernel_deliver` |
| `ended` | `kernel_id`, `reason` (`exited` or `lost`) | `ganymede_kernel_ended` |

From the server:

| `op` | Fields | C ABI equivalent |
| --- | --- | --- |
| `welcome` | or `error`, then the connection closes | none |
| `registered` | `name`, or `name` and `error` | `register`'s return value |
| `start` | `kernel_id`, `launch` (`env.JPY_SESSION_NAME` only) | `start` |
| `send` | `kernel_id`, `channel`, `msg` | `send` |
| `stop` | `kernel_id` | `stop` |

`started` exists because a frame can't return a value. The server waits
for it, within the kernel's startup timeout, before it waits for
`kernel_info_reply`. A `started` with an `error` fails the start at once,
as a nonzero return from the C ABI's `start` does, with no retry; so does
a provider that never answers, at the timeout. Both reach the API's
caller as a 500: the first names the provider's reason, the second says
the kernel did not start within the timeout. A `start` carries only the
session name: a provider runs the kernel where and with whatever
environment it likes, so no `cwd` or other `env` crosses the socket.

### Lifetime

- **A registration lasts as long as its connection.** When the
  connection closes, its kernelspecs are unregistered and each of its
  running kernels ends at once, dead with `"reason": "lost"`, with no
  restart: there is no one left to run it. A name a connection registered
  and then lost is never launched through the fallback provisioner or a
  kernel server for a kernel that was already running, but a later launch
  of the name goes to whatever lists it then, so a kernelspec installed
  on disk afterwards wins. A provider that reconnects registers again and
  gets new kernels; a kernel id from before never reaches it.
- **Ordering** is the C ABI's: per kernel, messages keep their order in
  both directions.
- **Backpressure.** The server's frames to a provider go through a bounded
  queue, 1024 frames by default. When it is full, or the provider takes
  nothing for 10 seconds, the server closes the connection rather than
  drop a frame, as the events socket does for a client that stops reading
  ([Hear of a write](api.md#hear-of-a-write)). Its kernels are then lost
  as above. A close and a `start` are ordered by the same lock, so a
  launch racing a disconnect never leaves a `started` answer pending.
- **Server restart** removes the socket file and closes every connection.
  The provider watches the runtime directory for the new
  `jpserver-<pid>.json` and connects again.

### What changes in each repository

- **ganymede**:
  - ganymede-server: the socket listener, its entry in the server info
    file, the `[kernel_providers]` config, the peer-user and token checks,
    and an `AppKernel` per connection.
  - `ganymede serve` builds a `RegisteredKernels` in front of its own
    kernels, as `ganymede-ffi` does; before this, only the embedded server
    had one.
  - ganymede-kernel-host, `RegisteredKernels`:
    - `unregister(name)`;
    - an origin per spec, so a socket's specs are listed and launched
      after the fallback's, and a lost kernel is never restarted
      through the fallback;
    - a start that waits for the provider's acknowledgement:
      `AppKernel::start` can't block, so `launch` gets an async step that
      awaits `started`;
    - an end with no restart for a provider that went away.
  - docs: docs/api.md (or docs/embedding.md) for the frames, and
    docs/security.md for the boundary.
  - Tests drive a fake provider over a real socket: hello, register,
    start, a message each way, interrupt, shutdown, a dropped connection,
    a reconnect, each refusal (other user, bad token, a name on disk,
    `argv` dropped, a reserved name), a full queue and a slow provider.
- **ganymede-ui**: when the app's notebooks go through the background
  server (#965 section 4), `AppKernels` and `ServerHost` register the
  TypeScript kernel there over the socket instead of through the C ABI.
  The kernel page and cdp-kernel don't change.

## Alternatives considered

- **A websocket route on the server**, guarded by the token and a loopback
  check. It reuses the existing listener, but the loopback check doesn't
  exclude proxies, and the token reaches browsers (see above). The first
  draft of this note proposed it.
- **A kernel server the app runs** (`gateway_url`). The background server
  would forward kernels to an HTTP kernel server inside the app. That
  needs the app to serve the whole kernels API and listen on a port of
  its own, which is more surface, not less.
- **No channel**: keep the in-app kernel on the embedded server only. The
  one-engine plan then can't route local notebooks through the
  background server without losing the in-app kernel from them.
