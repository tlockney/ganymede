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
`ganymede-engine = { git = "https://github.com/tlockney/ganymede", tag = "v0.3.0" }`.
What the notes below mark unreleased needs `branch = "main"` instead.

## The engine in a Rust app

`Engine` finds kernelspecs and brings kernels up; `Kernel` is one
running kernel. The crates underneath are re-exported as
`ganymede_engine::{protocol, nbformat, host, exec}`. The table lists
`Engine`'s methods down to `execute_notebook`, then `Kernel`'s.

| Call | What it does |
|---|---|
| `Engine::new()` | Kernelspecs from the Jupyter data dirs, kernels as subprocesses over ZeroMQ. |
| `Engine::builder()` | `.kernelspec_root(dir)`, `.kernelspecs(source)`, `.provisioner(p)`, `.startup_timeout(d)` (60 s), `.quiet(bool)`, `.build()`. |
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
| `run_notebook(&mut nb, &opts)`, `run_notebook_with(.., on_event)` | Run a `Notebook` in memory with `nbclient`'s semantics. |

`ExecuteResult` has `status`, `execution_count`, `outputs` as a notebook
stores them, the `execute_reply` content, and every iopub message.
`text()` is what a terminal would print; `error()` is `(ename, evalue)`.
`ExecuteOptions` defaults to a notebook cell's settings, with stdin off
and no timeout; a cell that runs past its `timeout` is interrupted.

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
    Usage(String),                      // a call this kernel or build can't serve
}
```

A failed cell is not an `Err`: `execute` returns `Ok` with `status`
`Error`. `Usage` is, for example, `restart` on an attached or adopted
kernel, or `start` with no provisioner.

### Streaming output and stdin

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

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

## Without ZeroMQ or subprocesses

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

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
| Browser wasm, WASI | off | `in_process_pair` and `adopt` | the engine builds for `wasm32-unknown-unknown` and runs a notebook under WASI (`e2e/wasm-smoke`) |

`ganymede-server` has the same two features, plus `gateway`. An app
embedding the server passes its own `kernelspecs`, `provisioner` and
`transports` in `serve::Startup`; built without a feature and given
none, `run_with` fails at startup. `ganymede-ffi` builds the server with
its default features, so the XCFramework has them on iOS too
([#208](https://github.com/tlockney/ganymede/issues/208)).

## The server in an app, from C

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

`ganymede-ffi` is a static library with four C functions, declared in
`crates/ganymede-ffi/include/ganymede.h`. The server runs on its own
thread with its own tokio runtime, so the app needs no Rust or async
code.

| Function | Contract |
|---|---|
| `GanymedeServer *ganymede_server_start(const char *config_json, char **error_out)` | Starts a server on loopback and blocks until it listens. On failure returns NULL and, if `error_out` isn't NULL, sets `*error_out` to a message. |
| `const char *ganymede_server_url(const GanymedeServer *server)` | The URL with its token. The server owns it: don't free it or use it after stop. |
| `void ganymede_server_stop(GanymedeServer *server)` | Gives open requests a moment, shuts down every kernel, stops the server and frees it. Blocks until done. NULL is ignored. |
| `void ganymede_string_free(char *s)` | Frees a string the library returned, such as the error. NULL is ignored. |

`config_json` is a JSON object with the keys of
[`ganymede.toml`](reference.md#config-file), or NULL. `port` defaults to
0, so the OS picks one; `ip` is `127.0.0.1` and the token is random, as
in `ganymede serve`. An unknown key is an error. Kernels are the
kernelspecs the config finds, or a kernel server's through `gateway_url`.
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
for `aarch64-apple-darwin` and `x86_64-apple-darwin` (joined by `lipo`),
`aarch64-apple-ios` and `aarch64-apple-ios-sim`, and writes
`target/xcframework/GanymedeFFI.xcframework` and a zip of it.
`MACOSX_DEPLOYMENT_TARGET` (13.0) and `IPHONEOS_DEPLOYMENT_TARGET` (16.0)
set the minimums. CI's macOS `ffi` job uploads the zip. Add the
framework to an Xcode target and import its module:

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

## Remote kernels with ganymede-gateway

> [!NOTE]
> **Unreleased.** Available only from a build of `main` (HEAD), not in
> the latest release.

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
