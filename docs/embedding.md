# Embedding

An application can take the Ganymede crates it needs instead of running
`ganymede` as a separate process. There are three ways in:

| Profile | Crate | What the app gets |
|---|---|---|
| A Rust app that runs code | `ganymede-engine` | Kernels and notebook runs as Rust calls. No HTTP. |
| An app with a web view | `ganymede-ffi` | The whole server on loopback, started from C or Swift. |
| No sockets or processes (wasm, an app's own kernel) | `ganymede-engine`, default features off | The same API over an in-process transport. |

`ganymede-gateway` gives the engine or the server kernels on another
Jupyter server. How the crates fit together is on
[Architecture](architecture.md#inside-ganymede); the app that drove this
design is on [The iPad use case](ipados.md).

![Three ways to embed Ganymede: a Rust app calling the engine in-process; an app whose web view talks HTTP to the server that ganymede-ffi runs on loopback; and a wasm or iOS build with zmq and subprocess off, where the kernel sits behind an in-process transport.](images/architecture/embedding.svg)

The crates are not on crates.io, so depend on them from git:
`ganymede-engine = { git = "https://github.com/tlockney/ganymede", tag = "v0.18.0" }`.

## The engine in a Rust app

`Engine` finds kernelspecs and brings kernels up; `Kernel` is one
running kernel. The crates underneath are re-exported as
`ganymede_engine::{protocol, nbformat, host, exec}`. The table lists
`Engine`'s methods down to `execute_notebook`, then `Kernel`'s.

| Call | What it does |
|---|---|
| `Engine::new()` | Kernelspecs from the Jupyter data dirs, kernels as subprocesses over ZeroMQ. |
| `Engine::builder()` | `.kernelspec_root(dir)`, `.kernelspecs(source)`, `.provisioner(p)`, `.startup_timeout(d)` (60 s), `.quiet(bool)`, `.prefix_output(bool)`, `.build()`. |
| `kernelspecs()`, `kernelspec(name)` | List, or look one up ignoring case. |
| `start(name)`, `start_with(name, StartOptions { cwd, env })` | Launch a kernelspec and wait for `kernel_info_reply`. |
| `attach(path)`, `connect(info)` | Use a running kernel by connection file or `ConnectionInfo`, as `jupyter console --existing`. A bare name is looked up in the Jupyter runtime dir; an empty path picks the newest. |
| `adopt(transport)`, `adopt_launched(launched, transport)` | Use a kernel the app drives itself. |
| `execute_notebook(path, kernel, &opts)` | Run a notebook file on a fresh kernel in its directory, without writing the file. Returns the notebook and the run's result separately, so outputs come back when a cell fails. |
| `kernel.execute(code)`, `execute_with(code, &opts, &mut handler)` | Run a cell; the second streams messages to a `Handler` and answers input through it. |
| `complete(code, pos)`, `inspect(code, pos, detail)` | The reply content as JSON (`matches`, `cursor_start`, `cursor_end`; `found`, `data`). |
| `is_complete(code)` | `Complete`, `Incomplete { indent }`, `Invalid` or `Unknown`. |
| `comm_info`, `comm_open`, `comm_msg`, `comm_close` | Comms from the frontend side. |
| `interrupt()` | A signal or `interrupt_request`, as the kernelspec says. |
| `restart()` | Relaunch from the kernelspec, keeping the working directory and environment. Only for kernels from `start`. |
| `shutdown()` | Ask the kernel to exit, then kill it if the engine launched it. Idempotent. |
| `run_notebook(&mut nb, &opts)`, `run_notebook_with(.., &mut handler)` | Run a `Notebook` in memory with `nbclient`'s semantics; the second reports progress and outputs to a `NotebookHandler`. |

Unless `quiet`, a kernel process's own stdout and stderr (not its cell
output) go to the app's stderr. `prefix_output(true)` starts each of
those lines with `[kernel <first 8 of its id>] `, so several kernels'
output can be told apart.

`ExecuteResult` has `status`, `execution_count`, `outputs` as a notebook
stores them, the `execute_reply` content, and every iopub message.
`text()` is what a terminal would print; `error()` is `(ename, evalue)`.
`ExecuteOptions` defaults to a notebook cell's settings, with stdin off
and no timeout; a cell that runs past its `timeout` is interrupted. A
request still queued behind another client's cell when it times out is
not: the interrupt would stop that cell instead.

A timed-out cell that started fails with `Host(Timeout)`; one still
queued fails with `Queued`, and the kernel runs it when its turn comes.

`Kernel` is `Send + Sync`: put it in an `Arc` to interrupt from one task
while another executes. A kernel the engine launched is killed when the
`Kernel` is dropped. Calls return `ganymede_engine::Result<T>`, except
`run_notebook`, which returns the runner's own `ganymede_exec::Error`:

```rust
pub enum Error {
    Host(ganymede_kernel_host::Error),  // launch, transport, timeout, a dead kernel
    Notebook(ganymede_nbformat::Error), // reading or parsing a notebook
    Exec(ganymede_exec::Error),         // a notebook cell that failed or timed out
    Io(std::io::Error),
    Queued(std::time::Duration),        // timed out before the kernel started the cell
    Usage(String),                      // a call this kernel or build can't serve
}
```

A failed cell is not an `Err`: `execute` returns `Ok` with `status`
`Error`. `Usage` is, for example, `restart` on an attached or adopted
kernel, or `start` with no provisioner.

### Streaming output and stdin

A `Handler` gets each iopub message for the request except `status` as
it arrives, and answers input requests. `on_input` is async, so a UI can
wait for the user without blocking the runtime; `None` answers with an
empty string. Messages a slow handler missed are delivered after the
reply. Implementing it needs the `async-trait` crate.

This uses the built-in fake kernel, which needs no Node or Python. Its
cell `input` asks for a line and echoes it, and `raise` fails:

```rust
use async_trait::async_trait;
use ganymede_engine::host::in_process_pair;
use ganymede_engine::host::testing::{FakeKernel, ReplyOrder};
use ganymede_engine::protocol::Message;
use ganymede_engine::{Engine, Error, ExecuteOptions, Handler};

struct Terminal;

#[async_trait]
impl Handler for Terminal {
    fn on_iopub(&mut self, msg: &Message) {
        if msg.msg_type() == "stream" {
            print!("{}", msg.content["text"].as_str().unwrap_or_default());
        }
    }
    async fn on_input(&mut self, prompt: &str, _password: bool) -> Option<String> {
        println!("[prompt {prompt:?}]");
        Some("hello".into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // With a kernelspec installed: Engine::new().start("cdp-node").await?
    let (host, side) = in_process_pair(64);
    FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
    let kernel = Engine::new().adopt(host).await?;

    let opts = ExecuteOptions { allow_stdin: true, ..Default::default() };
    let r = kernel.execute_with("input", &opts, &mut Terminal).await?;
    println!("status: {:?}, count: {:?}", r.status, r.execution_count);
    println!("error: {:?}", kernel.execute("raise").await?.error());
    println!("matches: {}", kernel.complete("ec", 2).await?["matches"]);
    if let Err(Error::Usage(m)) = kernel.restart().await {
        println!("restart: {m}");
    }
    kernel.shutdown().await
}
```

It prints:

```text
[prompt "? "]
you said: hello
status: Ok, count: Some(1)
error: Some(("FakeError", "raised"))
matches: ["echo"]
restart: restart needs a kernel this engine launched
```

### Streaming a notebook run

`run_notebook_with` takes an `exec::NotebookHandler`. Its `on_event`
gets `CellStarted`, `CellFinished`, and an `Output` for each iopub
message once it is written into the notebook's cells. `Output` names
the cells whose outputs changed, which includes earlier cells when a
display is updated. A closure taking an `exec::Event` is a handler.

Kernels may ask for input only when `NotebookOptions::allow_stdin` is
set; it is off by default, as in `nbclient`. `on_input` gets the cell's
index and the prompt, and answers as `Handler::on_input` does.

To run cells one at a time, as an editor does, keep one
`exec::NotebookRun` and call `execute_cell_with(&kernel.session(),
&mut nb.cells, index, &mut handler)` for each. The run carries the
execution count and display ids between calls, so a cell that updates a
display shown by an earlier cell rewrites that cell's output too.

## Without ZeroMQ or subprocesses

`ganymede-engine` has two features, both on by default:

| Feature | Brings in | Without it, `start` needs |
|---|---|---|
| `zmq` | The ZeroMQ transport, the default transport factory, `attach` and `connect` | `.transports(factory)` on the builder, else `Usage("no transport factory: ...")` |
| `subprocess` | `SubprocessProvisioner`, the default, which launches kernelspecs as child processes | `.provisioner(p)`, else `Usage("no provisioner: ...")` |

With both off, the engine is the session state machine, the in-process
transport, kernelspec parsing and the notebook runner. In their place,
`host::in_process_pair(capacity)` returns a `KernelTransport` for the
host and a `KernelSide` (`incoming`, `outgoing`) for whatever runs
cells. Adopt the transport, as above, or give the engine a provisioner
and transport factory, so `start` and `restart` work:

```rust
#[async_trait]
impl Provisioner for App {
    // No process to start: in_process() has no connection info, and
    // interrupts go over the control channel.
    async fn launch(&self, _: &Kernelspec, _: LaunchOptions) -> Result<LaunchedKernel> {
        Ok(in_process())
    }
}

#[async_trait]
impl TransportFactory for App {
    async fn connect(&self, _: &LaunchedKernel) -> Result<KernelTransport> {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply); // the app's kernel here
        Ok(host)
    }
}
```

Pass `Arc::new(App)` to the builder's `.provisioner` and `.transports`,
and a `KernelspecSource` listing the kernel to `.kernelspecs`, and
`engine.start(name)` works as with subprocesses.

| Profile | `zmq`, `subprocess` | Kernels | What CI checks |
|---|---|---|---|
| Desktop or server | on | kernelspecs as child processes, attached kernels, a gateway | everything |
| iOS or Android app, with processes | on | as on desktop, where the platform allows them | the engine, api and contents build for `aarch64-apple-ios` and `aarch64-linux-android` |
| iOS app, in-app kernel | off | the app's own provisioner and transport factory, or a gateway | the api builds for `aarch64-apple-ios`; the engine starts and restarts an in-process kernel; the server serves the app's own kernels |
| Browser wasm, WASI | off | `in_process_pair` and `adopt` | the engine, api and contents build for `wasm32-unknown-unknown`; the engine runs a notebook under WASI (`e2e/wasm-smoke`) |

`ganymede-server` has the same two features, plus `gateway`. An app
embedding the server passes its own `kernelspecs`, `provisioner` and
`transports` in `serve::Startup`; built without a feature and given
none, `run_with` fails at startup.

`ganymede-ffi` has the same three features, on by default. The macOS slice
of the XCFramework keeps them, and the iOS slices are built with
`--no-default-features --features gateway`. Built without `zmq` or
`subprocess`, the server in `ganymede-ffi` lists no kernels and refuses to
start one, rather than failing to start, unless `gateway_url` is set. An
app's own kernels go through the C functions instead
([Kernels the app runs](#kernels-the-app-runs)).

## The server in an app, from C

`ganymede-ffi` is a static library with these C functions, declared in
`crates/ganymede-ffi/include/ganymede.h`. The server runs on its own
thread with its own tokio runtime, so the app needs no Rust or async
code.

| Function | Contract |
|---|---|
| `GanymedeServer *ganymede_server_start(const char *config_json, char **error_out)` | Starts a server on loopback and blocks until it listens, or for at most 30 seconds, then fails. On failure returns NULL and, if `error_out` isn't NULL, sets `*error_out` to a message. |
| `const char *ganymede_server_url(const GanymedeServer *server)` | The URL with its token. The server owns it: don't free it or use it after stop. |
| `void ganymede_server_stop(GanymedeServer *server)` | Gives open requests a moment, shuts down every kernel, stops the server and frees it. Blocks until done. NULL is ignored. With registered kernels whose callbacks need the main thread (Swift's hop to the main actor), call it off the main thread, or each such kernel waits out the shutdown grace period. |
| `int ganymede_server_add_machine(GanymedeServer *server, const char *id, const char *url, const char *token, char **error_out)` | Mounts the Ganymede server at `url` as machine `id` (see [Connected machines](#connected-machines)). `token` may be NULL. Returns 0, or nonzero for a bad `id` or `url`, and then `*error_out` holds a message. Any thread. |
| `int ganymede_server_remove_machine(GanymedeServer *server, const char *id)` | Unmounts machine `id` and closes its open websockets. Its requests still waiting for an answer end with a 502, and its response bodies still streaming end with an error. Returns 1 if it was mounted, 0 if not. Any thread. |
| `char *ganymede_server_machines(const GanymedeServer *server)` | The ids of the machines mounted now, sorted, as a JSON array of strings (`["laptop","mini"]`, or `[]`). Free it with `ganymede_string_free`. NULL when `server` is NULL. Any thread. |
| `char *ganymede_config_read(const char *dir, char **error_out)` | The keys of `ganymede.toml` in `dir` (NULL: Ganymede's config dir) as a JSON object, `"{}"` when there is no file, for passing to `ganymede_server_start`. The file's keys as they are, not checked against the server's. Free it with `ganymede_string_free`. NULL on failure, and then `*error_out` holds a message. Any thread; no server needed. |
| `int ganymede_config_set(const char *dir, const char *json, char **error_out)` | Applies `json`, a JSON object, to `ganymede.toml` in `dir` (NULL: Ganymede's config dir): a value sets a key, `null` removes it, an object merges into that table. Comments, order and other keys are kept, and the file is replaced atomically with mode 0600. An edit that would make the config invalid is refused and the file is left as it was. Returns 0, or nonzero with `*error_out` set. Any thread; no server needed. |
| `void ganymede_string_free(char *s)` | Frees a string the library returned, such as the error, the list of machines or the config's keys. NULL is ignored. |
| `void ganymede_set_panic_callback(GanymedePanicCallback cb)` | Calls `cb(message)` on the panicking thread after any panic in the library, with `panicked at <file>:<line>:<col>: <message>`, valid during the call. For the app's crash reporter: a panic in a server task doesn't end the app, so this is how it hears of one. NULL removes it. The library sends nothing itself. |
| `void ganymede_debug_panic(void)` | Panics on the calling thread, which aborts the process. For testing crash reports. |

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

`ganymede_config_read` and `ganymede_config_set` let an app keep its
settings in the file `ganymede serve` reads. The app reads `ganymede.toml`
with `ganymede_config_read`, adds the keys only its launch needs
(`root_dir`, `static_dir`, `port`, `cookie_secret_file`), and passes the
JSON to `ganymede_server_start`, so the embedded server and
`ganymede serve` see the same settings. It writes a setting with
`ganymede_config_set`, so the file keeps the comments, order and other
keys a person wrote, and the server reads the change on its next start:
the app never writes TOML. With a NULL `dir`, both follow the file
`ganymede serve` reads: when that is still the old one in the Jupyter
config dir, `ganymede_config_read` reads it and `ganymede_config_set`
refuses, saying to move it, since a new file would make the server ignore
it. Calls in one process take turns; two processes writing at once can
lose one edit.

`config_json` is a JSON object with the keys of
[`ganymede.toml`](reference.md#config-file), or NULL. `port` defaults to
0, so the OS picks one; `ip` is `127.0.0.1` and the token is random, as
in `ganymede serve`. An unknown key is an error. Kernels are the
kernelspecs the config finds, or a kernel server's through `gateway_url`;
a build without `zmq` or `subprocess`, such as the iOS slices, has only
the latter (see above).
Strings passed in are UTF-8 and stay the caller's. The embedded server
installs no signal handlers and prints nothing to stdout.

A call sequence, run on Linux against a release build of the library:

```c
#include <stdio.h>
#include "ganymede.h"

int main(void) {
    char *err = NULL;
    GanymedeServer *server = ganymede_server_start(
        "{\"root_dir\": \"notebooks\"}", &err);
    if (!server) {
        fprintf(stderr, "ganymede: %s\n", err);
        ganymede_string_free(err);
        return 1;
    }
    printf("%s\n", ganymede_server_url(server)); /* load in the web view */
    ganymede_server_stop(server);
    return 0;
}
```

It prints `http://127.0.0.1:42549/?token=...`. With a missing `root_dir`
the error is `root dir /no/such/dir: No such file or directory (os error 2)`.
`scripts/ffi-smoke.sh` builds the library and links `tests/smoke.c`
with the system libraries `rustc --print native-static-libs` names.

### From Swift

On macOS with Xcode, `scripts/build-xcframework.sh` builds the library
for `aarch64-apple-darwin` (Apple silicon only), `aarch64-apple-ios` and
`aarch64-apple-ios-sim`, and writes
`target/xcframework/GanymedeFFI.xcframework` and a zip of it.
`MACOSX_DEPLOYMENT_TARGET` (26.0) and `IPHONEOS_DEPLOYMENT_TARGET` (26.0)
set the minimums.

Each release, and the `main-build` prerelease, carries the zip as
`GanymedeFFI.xcframework.zip` with `GanymedeFFI.xcframework.zip.sha256`
beside it, built by `release.yml` with the release profile. Besides
the framework, the zip holds `LICENSE` and `THIRD-PARTY-NOTICES` for
the crates compiled into it:

```sh
gh release download vX.Y.Z --repo tlockney/ganymede -p 'GanymedeFFI.xcframework.zip*'
shasum -a 256 -c GanymedeFFI.xcframework.zip.sha256
```

Add the framework to an Xcode target and import its module:

```swift
import GanymedeFFI

var err: UnsafeMutablePointer<CChar>?
guard let server = ganymede_server_start(#"{"root_dir": "/path/to/notebooks"}"#, &err) else {
    let message = err.map { String(cString: $0) } ?? "unknown error"
    ganymede_string_free(err)
    fatalError(message)
}
let url = URL(string: String(cString: ganymede_server_url(server)))!
// Load url in a WKWebView; at quit, ganymede_server_stop(server).
```

ganymede-ui's app does this in `ServerHost.swift`; see its
[architecture](ganymede-ui/architecture.md#mac-and-ipad-app).

### Kernels the app runs

An app that runs a kernel itself, as the iPad app runs cdp-kernel in a
web view, registers it with the server. The server lists it as a
kernelspec after the gateway's kernels or its own, in place of one of
the same name (so the default kernel stays the server's own unless it
has none), and runs it through the same sessions, rooms and execute
endpoint as any kernel. The design is
[In-app kernels as server kernels](in-app-kernel.md).

| Function | Contract |
|---|---|
| `int ganymede_kernel_register(GanymedeServer *server, const char *spec_json, const GanymedeKernelCallbacks *callbacks, char **error_out)` | Lists `spec_json` (`kernel.json`'s keys plus `"name"`; `argv` may be left out) as a kernelspec the callbacks run. The name is lowercased and, as on disk, takes only `[a-z0-9._-]`. Returns 0, or nonzero for a bad spec or name, a NULL `start`, `send` or `stop`, or a name already registered; then no callback is ever called, `release` included, and `*error_out` holds a message. |
| `int ganymede_kernel_deliver(GanymedeServer *server, const char *kernel_id, const char *channel, const char *msg_json)` | A message from the kernel on `shell`, `control`, `stdin` or `iopub`. Any thread; doesn't block. Returns 0; 1 for a NULL string or a message or channel it can't read; 2 when the kernel isn't running, as after a stop. |
| `void ganymede_kernel_ended(GanymedeServer *server, const char *kernel_id, const char *reason)` | The kernel ended without being asked to. `"lost"` (the platform ended it) restarts it, unless restarts are off (`kernel_restart_limit` 0), counted apart from crashes (a loss before the kernel first answers is a failed start, retried as one), and the `status: restarting` message carries `"reason": "lost"`. Anything else is a crash. Call it once per run, never for a run already stopped. |

A kernel lost 4 times in a row, each within 60 seconds of the last, is left
dead, and `status: dead` carries `"reason": "lost"`
([details](reference.md#when-the-platform-ends-a-kernel)).

`GanymedeKernelCallbacks` holds `ctx` and four callbacks, each passed
`ctx`:

- `start(ctx, kernel_id, launch_json)` starts a kernel and returns 0 once
  the start is under way; the server then waits for its
  `kernel_info_reply`, as for any kernel. `launch_json` is
  `{"kernelspec", "cwd", "env"}`, and `env.JPY_SESSION_NAME` is the
  notebook's path for a notebook's kernel. A restart calls it again with
  the same id.
- `send(ctx, kernel_id, channel, msg_json)` hands the kernel a message on
  `shell`, `control` or `stdin`.
- `stop(ctx, kernel_id)` ends it. A shutdown sends `shutdown_request`
  first. A restart reuses the id, so once `stop` is called, or the app
  called `ganymede_kernel_ended`, the app delivers nothing more from that
  run: a late message would reach the new one.
- `release(ctx)`, which may be NULL, runs once when the server stops,
  after every other callback.

The server calls them on its own threads, never the app's main thread,
and maybe at once for different kernels, so they must not block; Swift
hops to the main actor to reach a web view. Calls to `send` for one
kernel don't overlap and come in order. Messages are Jupyter JSON in the
websocket's shape (`header`, `parent_header`, `metadata`, `content`),
with binary buffers as base64 strings in `buffers`, and nothing is
signed. Interrupts go over the control channel as `interrupt_request`,
whatever the kernelspec says.

### Kernel providers

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

A program of the same user can register kernelspecs with a running
`ganymede serve` and run their kernels itself, which the C ABI above
can't do for a server in another process. The server opens a Unix socket
in the Jupyter runtime dir, `ganymede-kernels-<pid>.sock`, when
`[kernel_providers] enabled = true`, and names it in
`jpserver-<pid>.json` as `kernel_providers_socket`. Each connection is an
`AppKernel` behind the server's registered set, so its kernels run
through the same sessions, rooms and execute endpoint as any other. The
design is [Kernel providers](kernel-provider.md), and
[Security](security.md#kernel-providers) covers the boundary.

The socket is Unix only, off by default, and opens only when auth is on:
a client must come from this user's uid and say `hello` with the
server's token. While a connection holds it, it registers names, and a
name is first come: one registered elsewhere, or by another connection,
is refused. A name in `[kernel_providers] reserve` is the exception: only
a provider whose code signature meets the reservation's requirement
(on macOS) may register it, and anyone else gets `registered` with
`error` `kernelspec <name> is reserved for another program`.

Frames are newline-delimited JSON, one object per line, with an `op`.
Jupyter messages are the websocket's JSON shape, as the C ABI carries
them, with binary buffers as base64 strings in `buffers`. A line longer
than 64 MiB closes the connection.

From the provider:

| `op` | Fields | C ABI equivalent |
| --- | --- | --- |
| `hello` | `token` | none |
| `register` | `spec` (`name`, `display_name`, `language`, `metadata`; anything else, `argv` and `env` included, is dropped) | `ganymede_kernel_register` |
| `unregister` | `name` | none |
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

The first frame must be `hello`, within 10 seconds; anything else gets
`{"op": "welcome", "error": "..."}` and a closed connection. After
`welcome`, the connection stays open until either side closes it.

A start happens in two steps, because a frame can't return a value: the
server sends `start` and waits up to `startup_timeout_secs` for
`started`, then up to `startup_timeout_secs` again for the kernel's
`kernel_info_reply`, so a launch can wait twice that. A `started` with an
`error` fails the launch at once, with no retry.

The server's frames go through a queue of 1024, and a write that takes
more than 10 seconds closes the connection, as the events socket does for
a client that stops reading. When a connection closes for any reason,
its specs are forgotten and its running kernels end dead with reason
`lost`, never restarted: there is no one left to run them. A provider
that reconnects registers again and gets new kernels; a kernel id from
before never reaches it. The server keeps one small entry per kernel id a
provider started until its connection closes.

A restart reuses the kernel's id: the server sends `stop`, then `start`
with the same `kernel_id`. Frames carry only that id, so once a provider
has received `stop` it sends nothing more for the old run, no `deliver`
and no `ended`, until it has answered the next `start`: a late frame
would reach the new run.

### Connected machines

An app can show another computer running `ganymede serve` (a connected
machine) in the same windows as its own notebooks. The page in a window
only talks to its own origin, the embedded server, which forwards to the
machine, so the page makes no cross-origin requests and the machine's
credential never reaches it.

`ganymede_server_add_machine(server, "mini", "http://mini.example:8888/", token, &err)`
mounts the machine at `/machines/mini/`: a request to
`/machines/mini/api/contents/a.ipynb` is `GET /api/contents/a.ipynb` on
the machine, and a websocket to `/machines/mini/api/kernels/{id}/channels`
(with the `v1.kernel.websocket.jupyter.org` subprotocol, if the page
offers it) is the machine's. The `id` is 1 to 64 of `A-Z a-z 0-9 _ -`.
`ganymede_server_remove_machine` unmounts it again, and closes its open
websockets, which the page sees as a close with code 1001. Adding an `id`
that is mounted replaces it, so a changed URL or token takes effect and the
old sockets close. Nothing is mounted until the app adds a machine, and
none survives the server: the app adds its machines again at each start.
`ganymede_server_machines` lists the ids mounted now, so the app need not
keep its own list.

Removing or replacing a machine also ends its requests in flight: one
still waiting for the machine's answer is a 502 (`machine {id} was
removed`) at once, and a response body still streaming ends with an error
partway through (a download stops), as the machine's connection is
dropped. A request already sent may have reached the machine: a save or
other write that ends this way may or may not have been applied.

Only these paths below `/machines/{id}/` are forwarded; anything else is a
404, and a method other than GET, HEAD, PUT, POST, PATCH or DELETE is a
405 and is not forwarded; a path with `..`, an encoded slash or backslash, an empty segment
or a NUL is a 400:

- `api/status`, `api/me`, `api/contents`, `files`, `api/nbconvert` and
  `nbconvert`
- `api/collaboration` and `api/fileid` (rooms)
- `api/kernels` with its `channels` websocket, `api/sessions`,
  `api/kernelspecs`, and `kernelspecs/{name}/{file}` (a kernel's logo; one
  file name, nothing deeper)
- `api/terminals`, and `terminals/websocket/{name}`, the terminal's socket
- `api/notebooks/run`, `api/metrics/v1/kernel_usage`, `api/events/subscribe`
- `api/host/grants`, and everything below it: a notebook's access settings
  (`GET` to show it, `PUT` to set it, `DELETE` to remove it)

`mcp`, the rest of `api/host` and everything else on the machine are never
reachable this way. The rest of `api/host` is the cells' own host API, which
a machine's kernels call on the machine itself, not through this one. A
notebook's access settings add nothing to what the mount already does: it
forwards `api/kernels`, `api/terminals` and `api/notebooks/run` with the
machine's token, so a page that may call the mount can already run code
there, and a page the machine serves holds no token and runs in the sandbox,
so a notebook can't change its own grant through the mount.

A forwarded `api/kernelspecs` answer is the machine's own: its
`resources` are paths on the machine (`/kernelspecs/{name}/logo-64x64.png`
under the machine's base URL), not below the mount. To show a machine's
kernel logo, a page takes the part from `kernelspecs/` on and joins it
below `/machines/{id}/`; used as is, the path reaches this server's own
kernelspecs instead.

What the machine gets:

- `Authorization: token <token>` when the app gave one, and always
  `Ganymede-Client: ganymede-mount`, so a machine that lets this device in
  by [tailnet identity](security.md#tailnet-identity) (`[tailscale]
  allow`) accepts it as a program, with no token.
- Nothing of the page's. The `Cookie`, `Authorization`, `Origin`,
  `Referer`, `X-XSRFToken`, `Sec-Fetch-*` and proxy headers, a
  `Ganymede-Client` the page sent, and the `token` and `_xsrf` query
  parameters are removed first. Bodies pass unchanged, in both directions.

What the page gets back: the machine's status and body, and only the
headers that describe the body and its caching (`Content-Type`,
`Content-Length`, `Content-Disposition`, `Content-Encoding`,
`Content-Range`, `Accept-Ranges`, `Cache-Control`, `ETag`,
`Last-Modified`, `Expires`, `Vary`, `X-Content-Type-Options`). The
machine's cookies never reach the page. A `Location` that is a path on the
machine gets the `/machines/{id}` prefix (after any base URL), but only
when the path is one the mount would forward itself; a whole URL, a path
that climbs out of the mount (`/../..`, in any encoding) and a path off the
allowlist are dropped.
Every response that is not a websocket upgrade carries this server's own
`Content-Security-Policy`, whatever the route and whatever the machine
sent, since the machine's HTML lands on the app's origin: `sandbox
allow-scripts`, as its `/files` sends, and with `allow-downloads` for
`nbconvert/html/...`.

A machine that cannot be reached is a 502 with a `{"message": ...}` body (a
websocket is refused with it at the handshake), and one that accepts the
request but does not start answering within 30 seconds is a 504, and an `id` that is not
mounted is a 404. [Security](security.md#connected-machines) says who may
use the mount.

### Seeing the server's own writes

An app that keeps answers per notebook file (a link allowed "always", a
device permission) forgets them when the file is replaced outside
Ganymede, and keeps them when Ganymede writes it. It can stamp a file
itself, but most writes on its server come from the server, with no window
involved: a room's autosave, a run saving its outputs, MCP. So the app can
ask to be told of them:

```c
static void wrote(void *ctx, const char *path, const char *before, const char *after) {
    /* Move the answers kept for the file at path from the stamp before to the stamp after. */
}
static void done_with(void *ctx) { /* free ctx */ }
ganymede_server_set_write_observer(server, wrote, done_with, ctx);
```

After each write the server makes of a file under its root that succeeds,
the callback is called once with the file's canonical absolute path (links
resolved; on Windows it has the `\\?\` prefix) and its stamp just before
the write and just after it. A stamp is
`<size>:<modification time in ns>:<inode>` (no inode on Windows), the
same as `file_stamp` in `ganymede-contents` makes from the file's metadata,
so an app can stamp a file itself and compare. A stamp that can't be read
is NULL: `before` is NULL for a file the write made.

Reported: any successful save of a regular file, however it came (REST, a
room's autosave, a run that saves its outputs, MCP), a new file's included
(`before` is then NULL), a checkpoint restore, and a copy to a named
destination path, for the destination. Not reported: a write that failed; a
copy into a directory, a new untitled file, a rename, a delete, and a
directory save; a file changed by anyone but the server; and every write
when the root is not a local directory. Renames are not reported, so an app
that keys its answers by path loses them on a rename it did not make itself,
which fails safe. It is a function of the embedding, not a REST endpoint.

The callback runs on a thread of the server, never the main thread, and
maybe at once for different files, with the server's turn for that path
held: the next write of the same path, or a rename of it, waits for it, so
it should return quickly. The strings are valid only during the call. A
path with a NUL byte, or that is not UTF-8, is not reported. `ctx` is the
app's: it must be usable from any thread until `release`.

`release` (may be NULL) is called with `ctx` once, after the last call that
uses that `ctx` has returned and none follows: when the observer is
replaced or removed, or the server stopped. The app frees `ctx` there, and
not before. It runs on a server thread, or on the thread of the call that
replaced the observer or stopped the server, and must not block.

Calling the function again replaces the observer, and a NULL callback
removes it (`release` is then ignored). A write is reported to whatever is
set when it ends, so one that began before a replacement goes to the new
callback; one that began while none was set is not reported at all, as its
`before` was never taken. Nothing is reported until one is set, and none
survives the server.

From Rust, `ganymede_server::write_observer::WriteObserver` is the same
thing: keep one, pass it as `Startup::write_observer`, and call `set` with
a closure taking the path and the two stamps.

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

A client that doesn't embed the server — one talking to `ganymede serve`
over the API — hears the same writes on `/api/events/subscribe`
([Hear of a write](api.md#hear-of-a-write)), one event per write.

### Two servers on one notebook

An embedded server's rooms lock their notebooks as every Ganymede server's
do ([Rooms](rooms.md#two-servers-on-one-notebook)): the first server on the
machine with a room open on a file owns it, and another server's room on it
is read-only. The locks are files in the Jupyter runtime dir's
`ganymede-rooms/`, so the app and the CLI must see the same
`JUPYTER_RUNTIME_DIR` (or `JUPYTER_DATA_DIR`) for them to meet. An app
started from the Finder doesn't see variables set in a shell profile.

ganymede-ffi turns locking off on iOS, which has no shared runtime dir. From
Rust, `Startup::room_locks` decides it: `RoomLocks::RuntimeDir`, the
default, or `RoomLocks::Off`, which an embedder on iOS sets itself.

## Remote kernels with ganymede-gateway

`Gateway` is a kernelspec source, provisioner and transport factory in
one, for the kernels of another Jupyter or Ganymede server, as
`jupyter_server`'s gateway client has them. It needs neither `zmq` nor
`subprocess`, though it opens sockets of its own (tokio's `net`), so it
is not for the browser profile.

```rust
let gw = Arc::new(Gateway::new("https://kernels.example.org", Some(token))?);
gw.refresh().await?; // the kernelspec list is empty until this succeeds
let engine = Engine::builder()
    .kernelspecs(gw.clone())
    .provisioner(gw.clone())
    .transports(gw)
    .build();
let kernel = engine.start("cdp-node").await?;
```

`Gateway::new(url, token)` makes no request. `refresh_every(d)` refreshes
now and every `d` on the current tokio runtime; `remote_kernels()` lists
the remote kernel ids. Start, interrupt and shutdown go over the remote
REST API, a restart is a new remote kernel, and channels use its
websocket. TLS is rustls with the platform's trust store. The server
does the same for `gateway_url`; see [Gateway](reference.md#gateway).
