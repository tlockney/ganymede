//! A C ABI for running the Ganymede server inside an app.
//!
//! The Swift shell starts the server on loopback, loads the URL it gets
//! back in a web view, and stops the server when the app quits. The
//! server runs on its own thread with its own tokio runtime, so the app
//! needs no Rust or async code of its own. `include/ganymede.h` declares
//! these functions for C and Swift.
//!
//! Strings passed in are UTF-8 and owned by the caller. Strings returned
//! are owned by the caller and freed with [`ganymede_string_free`].
//!
//! An app can also run kernels itself and register them with the server
//! ([`ganymede_kernel_register`]), as the iPad app will with its in-app
//! kernel (#266, `docs/in-app-kernel.md`).
//!
//! An app can mount other Ganymede servers, its connected machines, under
//! `/machines/{id}/` ([`ganymede_server_add_machine`],
//! [`ganymede_server_remove_machine`]); the server forwards an allowlist of
//! routes to them for the app's own pages (#894, `docs/embedding.md`).
//!
//! An app that keeps answers per notebook file can watch the server's own
//! writes of them ([`ganymede_server_set_write_observer`]): the callback
//! gets each written file's path and its stamp before and after, so the
//! app can tell the server's writes from a file replaced outside (#916,
//! `docs/embedding.md`).
//!
//! Built without the `zmq` or `subprocess` feature, as the iOS slices
//! of the XCFramework are, the server can't run kernelspecs itself. It
//! then lists no kernels and refuses to start one, unless the config
//! sets `gateway_url`, whose kernels it runs as usual.

mod v8_bench;

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{mpsc, Arc, Once, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use async_trait::async_trait;
use ganymede_api::StaticKernelspecs;
use ganymede_kernel_host::protocol::{Channel, Message};
use ganymede_kernel_host::{
    AppKernel, Ended, Kernelspec, LaunchOptions, LaunchedKernel, Provisioner, RegisteredKernels,
    TransportFactory,
};
use ganymede_server::config::Config;
use ganymede_server::mount::Machines;
use ganymede_server::serve::{gateway_configured, run_with, Startup};
use ganymede_server::write_observer::WriteObserver;

/// A running server. Opaque to C.
pub struct GanymedeServer {
    url: CString,
    /// The kernels the app registers, which the server lists after its own.
    registered: Arc<RegisteredKernels>,
    /// The machines mounted under `/machines/{id}/`.
    machines: Arc<Machines>,
    /// Told of the server's writes of files.
    write_observer: Arc<WriteObserver>,
    stop: tokio::sync::mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

/// Starts a server and returns once it is listening, or failed to.
///
/// `config_json` is a JSON object with the keys of `ganymede.toml`
/// (`root_dir`, `static_dir`, `kernelspec_roots`, `token`, ...), or NULL
/// for the defaults. `port` defaults to 0 here, so the OS picks a free
/// one; `ip` defaults to `127.0.0.1` and `token` to a random one, as in
/// `ganymede serve`.
///
/// Gives up after [`START_TIMEOUT`] if the server has not listened by
/// then, and asks it to stop when it does.
///
/// Returns NULL on failure, and then, if `error_out` is not NULL, sets
/// `*error_out` to a message to free with [`ganymede_string_free`].
///
/// # Safety
///
/// `config_json` is NULL or a NUL-terminated string; `error_out` is NULL
/// or valid for a write.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_start(
    config_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut GanymedeServer {
    let json = if config_json.is_null() {
        None
    } else {
        // SAFETY: the caller passes a NUL-terminated string.
        Some(
            unsafe { CStr::from_ptr(config_json) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    match start(json.as_deref()) {
        Ok(server) => Box::into_raw(Box::new(server)),
        Err(e) => {
            if !error_out.is_null() {
                // SAFETY: the caller passes a pointer valid for a write.
                unsafe { *error_out = into_c(format!("{e:#}")) };
            }
            std::ptr::null_mut()
        }
    }
}

/// The server's URL with its token, such as
/// `http://127.0.0.1:52011/?token=...`. Valid until the server is stopped;
/// do not free it.
///
/// # Safety
///
/// `server` came from [`ganymede_server_start`] and was not stopped.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_url(server: *const GanymedeServer) -> *const c_char {
    // SAFETY: the caller passes a live server.
    unsafe { &*server }.url.as_ptr()
}

/// Mounts the Ganymede server at `url` as machine `id`: requests from
/// the app's own pages to `/machines/{id}/...` on this server go to it
/// (HTTP and websocket, an allowlist of routes), with `token` added, or,
/// when `token` is NULL, with only a `Ganymede-Client` header, which a
/// machine that lets this device in by tailnet identity accepts. The
/// page's own cookie, token and `Origin` are never forwarded. Nothing is
/// mounted until this is called; the token is never given to the page.
///
/// `id` is 1 to 64 of `A-Z a-z 0-9 _ -`; `url` is `http(s)://host[:port]
/// [/base]`. No request is made now. Adding an `id` that is mounted
/// replaces it (a changed URL or token) and closes its open websockets.
/// Takes any thread.
///
/// Returns 0 on success. On failure (a bad id or URL) returns nonzero and,
/// if `error_out` is not NULL, sets `*error_out` to a message to free with
/// [`ganymede_string_free`].
///
/// # Safety
///
/// `server` is live; `id` and `url` are NUL-terminated strings; `token` is
/// NULL or one; `error_out` is NULL or valid for a write.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_add_machine(
    server: *mut GanymedeServer,
    id: *const c_char,
    url: *const c_char,
    token: *const c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    // SAFETY: the caller passes a live server and strings or NULL.
    let (server, id, url, token) = unsafe { (&*server, from_c(id), from_c(url), from_c(token)) };
    let result = match (id, url) {
        (Some(id), Some(url)) => server.machines.add(&id, &url, token.as_deref()),
        _ => Err("id and url must not be NULL".to_string()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            if !error_out.is_null() {
                // SAFETY: the caller passes a pointer valid for a write.
                unsafe { *error_out = into_c(e) };
            }
            1
        }
    }
}

/// Unmounts machine `id` and closes its open websockets; requests to
/// `/machines/{id}/...` then get 404. Returns 1 if it was mounted, 0 if
/// not (or `id` is NULL). Takes any thread.
///
/// # Safety
///
/// `server` is live; `id` is NULL or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_remove_machine(
    server: *mut GanymedeServer,
    id: *const c_char,
) -> c_int {
    // SAFETY: the caller passes a live server and a string or NULL.
    let (server, id) = unsafe { (&*server, from_c(id)) };
    match id {
        Some(id) => c_int::from(server.machines.remove(&id)),
        None => 0,
    }
}

/// Called after the server writes a file: `path` is the file's absolute
/// path, `before` and `after` its stamp (size, modification time in
/// nanoseconds and, on Unix, inode, as `ganymede_contents::file_stamp`
/// makes it) just before and just after the write, or NULL when it could
/// not be read, as `before` for a file that is new. All three strings are
/// valid only during the call.
pub type GanymedeWriteCallback = extern "C" fn(
    ctx: *mut c_void,
    path: *const c_char,
    before: *const c_char,
    after: *const c_char,
);

/// An app's write callback and its context, as the server calls them.
struct CWriteObserver {
    ctx: *mut c_void,
    callback: GanymedeWriteCallback,
}

// SAFETY: the C contract (`ganymede_server_set_write_observer`) has the
// app accept calls with `ctx` on any of the server's threads, until the
// observer is replaced or the server stopped; the server never touches
// what `ctx` points to.
unsafe impl Send for CWriteObserver {}
// SAFETY: as above; concurrent calls are part of the same contract.
unsafe impl Sync for CWriteObserver {}

impl CWriteObserver {
    fn call(&self, path: &std::path::Path, before: Option<&str>, after: Option<&str>) {
        // A path or stamp with a NUL can't be passed as a C string; no
        // file name has one on the platforms the apps run on, so the call
        // is skipped rather than given a different path.
        let Ok(path) = CString::new(path.to_string_lossy().into_owned()) else {
            return;
        };
        let stamp = |s: Option<&str>| s.and_then(|s| CString::new(s).ok());
        let (before, after) = (stamp(before), stamp(after));
        let ptr = |s: &Option<CString>| s.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        (self.callback)(self.ctx, path.as_ptr(), ptr(&before), ptr(&after));
    }
}

/// Calls `callback` with `ctx` after each write the server makes of a
/// file under its root: a save from the REST API, a room's autosave, a
/// run saving its outputs, MCP, and a copy over an existing file. Not
/// reported: a write that failed, a rename, a delete, a new file (a copy
/// into a directory, a new untitled file), and any write when the root is
/// not a local directory. Reported once per write, after it, with the
/// file's stamp just before it was written and just after, so the app can
/// move what it keeps for the file from the one stamp to the other, and
/// still tell a file replaced outside from a write of the server's. A
/// stamp that can't be read, such as `before` for a new file, is NULL.
///
/// The callback runs on a thread of the server, never the main thread,
/// maybe at once for different files; it must not block. `ctx` must be
/// usable from any thread until the observer is replaced or the server
/// stopped: a call already under way may still use the old `ctx` when
/// this returns. A path with a NUL byte is not reported. NULL `callback`
/// removes the observer. Takes any thread, and replaces the one set before;
/// it takes effect for writes that begin after the call.
///
/// # Safety
///
/// `server` is live; `callback` is NULL or a function the app accepts calls
/// of on any thread, with `ctx`, as above.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_set_write_observer(
    server: *mut GanymedeServer,
    callback: Option<GanymedeWriteCallback>,
    ctx: *mut c_void,
) {
    // SAFETY: the caller passes a live server.
    let server = unsafe { &*server };
    server.write_observer.set(callback.map(|callback| {
        let observer = CWriteObserver { ctx, callback };
        Arc::new(
            move |path: &std::path::Path, before: Option<&str>, after: Option<&str>| {
                observer.call(path, before, after)
            },
        ) as Arc<ganymede_server::write_observer::WriteObserverFn>
    }));
}

/// Stops the server: open requests get a moment to finish, every kernel
/// is shut down, and the call returns when that is done. Frees `server`.
/// NULL is ignored.
///
/// # Safety
///
/// `server` came from [`ganymede_server_start`] and is not used again.
#[no_mangle]
pub unsafe extern "C" fn ganymede_server_stop(server: *mut GanymedeServer) {
    if server.is_null() {
        return;
    }
    // SAFETY: the caller hands back ownership of a live server.
    let mut server = unsafe { Box::from_raw(server) };
    let _ = server.stop.try_send(());
    if let Some(t) = server.thread.take() {
        let _ = t.join();
    }
    // The server's runtime ended with its thread, so this is the last
    // reference: each registration's `release` runs now, after every
    // other callback.
    drop(server);
}

/// Frees a string this library returned. NULL is ignored.
///
/// # Safety
///
/// `s` came from this library and is not used again.
#[no_mangle]
pub unsafe extern "C" fn ganymede_string_free(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: the string was made by `CString::into_raw`.
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Called with a panic's message and location, such as
/// `panicked at crates/x/src/y.rs:10:5: oops`, on the panicking thread.
/// The string is valid only during the call.
pub type GanymedePanicCallback = extern "C" fn(message: *const c_char);

/// The callback [`ganymede_set_panic_callback`] installed.
static PANIC_CALLBACK: RwLock<Option<GanymedePanicCallback>> = RwLock::new(None);
static PANIC_HOOK: Once = Once::new();

/// Calls `callback` on every panic in this library, after the default
/// hook (which prints the panic to stderr), on the panicking thread: the
/// app's crash reporter can then report it with the thread's stack,
/// Rust frames included (ganymede-ui#318). Panics in the server's tasks
/// don't end the app, so this is how the app hears of them. NULL removes
/// the callback. The library sends nothing itself.
#[no_mangle]
pub extern "C" fn ganymede_set_panic_callback(callback: Option<GanymedePanicCallback>) {
    if let Ok(mut slot) = PANIC_CALLBACK.write() {
        *slot = callback;
    }
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            // Copied out, so the lock isn't held during the call.
            let callback = PANIC_CALLBACK.read().ok().and_then(|slot| *slot);
            if let Some(callback) = callback {
                let message =
                    CString::new(panic_message(info).replace('\0', " ")).expect("NULs replaced");
                callback(message.as_ptr());
            }
        }));
    });
}

/// `panicked at <file>:<line>:<column>: <payload>`, as the default hook
/// prints it.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("Box<dyn Any>");
    match info.location() {
        Some(l) => format!(
            "panicked at {}:{}:{}: {payload}",
            l.file(),
            l.line(),
            l.column()
        ),
        None => format!("panicked: {payload}"),
    }
}

/// Panics on the calling thread, for checking crash reports, then aborts
/// the process. The panic is caught before it reaches the `extern "C"`
/// boundary, where std would panic a second time ("panic in a function
/// that cannot unwind") and run the hook again: so the callback runs once.
#[no_mangle]
#[inline(never)]
pub extern "C" fn ganymede_debug_panic() {
    let _ =
        std::panic::catch_unwind(|| panic!("ganymede_debug_panic: a test panic the app asked for"));
    std::process::abort();
}

/// An app's callbacks for a kernel it runs itself. See
/// [`ganymede_kernel_register`] for when each is called.
#[repr(C)]
pub struct GanymedeKernelCallbacks {
    /// Passed to each callback; the app's until `release`.
    pub ctx: *mut c_void,
    /// Start kernel `kernel_id`. `launch_json` is
    /// `{"kernelspec": name, "cwd": path or null, "env": {name: value}}`;
    /// `env` has `JPY_SESSION_NAME`, the notebook's path, when the kernel
    /// is a notebook's. Return 0 once the start is under way, nonzero to
    /// fail it.
    pub start: Option<
        extern "C" fn(
            ctx: *mut c_void,
            kernel_id: *const c_char,
            launch_json: *const c_char,
        ) -> c_int,
    >,
    /// A Jupyter message for the kernel, as JSON, on `"shell"`,
    /// `"control"` or `"stdin"`.
    pub send: Option<
        extern "C" fn(
            ctx: *mut c_void,
            kernel_id: *const c_char,
            channel: *const c_char,
            msg_json: *const c_char,
        ),
    >,
    /// End the kernel now.
    pub stop: Option<extern "C" fn(ctx: *mut c_void, kernel_id: *const c_char)>,
    /// No callback follows: `ctx` may be freed. NULL if there is nothing
    /// to free.
    pub release: Option<extern "C" fn(ctx: *mut c_void)>,
}

/// A registration's callbacks, as the server calls them.
struct CKernel {
    ctx: *mut c_void,
    start: extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    send: extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char),
    stop: extern "C" fn(*mut c_void, *const c_char),
    release: Option<extern "C" fn(*mut c_void)>,
}

// SAFETY: the C contract (`ganymede_kernel_register`) has the app accept
// calls with `ctx` on any of the server's threads; the server never
// touches what `ctx` points to.
unsafe impl Send for CKernel {}
// SAFETY: as above; concurrent calls are part of the same contract.
unsafe impl Sync for CKernel {}

impl Drop for CKernel {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            release(self.ctx);
        }
    }
}

/// `s` as a C string, with any NUL replaced, for the length of a call.
fn c_str(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).expect("NULs replaced")
}

impl AppKernel for CKernel {
    fn start(
        &self,
        kernel_id: &str,
        spec: &Kernelspec,
        opts: &LaunchOptions,
    ) -> Result<(), String> {
        let launch = serde_json::json!({
            "kernelspec": spec.name,
            "cwd": opts.cwd.as_ref().map(|p| p.to_string_lossy()),
            "env": opts
                .env
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::from(v.as_str())))
                .collect::<serde_json::Map<_, _>>(),
        });
        let (id, launch) = (c_str(kernel_id), c_str(&launch.to_string()));
        match (self.start)(self.ctx, id.as_ptr(), launch.as_ptr()) {
            0 => Ok(()),
            n => Err(format!("its start callback returned {n}")),
        }
    }

    fn send(&self, kernel_id: &str, channel: Channel, msg: &Message) {
        let json = match message_json(msg) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(kernel = %kernel_id, "message not sent to the app's kernel: {e}");
                return;
            }
        };
        let (id, channel, json) = (c_str(kernel_id), c_str(channel.as_str()), c_str(&json));
        (self.send)(self.ctx, id.as_ptr(), channel.as_ptr(), json.as_ptr());
    }

    fn stop(&self, kernel_id: &str) {
        let id = c_str(kernel_id);
        (self.stop)(self.ctx, id.as_ptr());
    }
}

/// `msg` in the websocket's JSON shape, with its buffers as base64
/// strings in `buffers`.
fn message_json(msg: &Message) -> anyhow::Result<String> {
    use base64::Engine;
    let mut v = serde_json::to_value(msg)?;
    v["buffers"] = msg
        .buffers
        .iter()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b).into())
        .collect::<Vec<serde_json::Value>>()
        .into();
    Ok(serde_json::to_string(&v)?)
}

/// The other way: base64 strings in `buffers` become the buffers.
fn parse_message(json: &str) -> anyhow::Result<Message> {
    use base64::Engine;
    let mut v: serde_json::Value = serde_json::from_str(json)?;
    let buffers = match v.as_object_mut().and_then(|m| m.remove("buffers")) {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|b| {
                let text = b
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("a buffer is not a base64 string"))?;
                Ok(base64::engine::general_purpose::STANDARD
                    .decode(text)?
                    .into())
            })
            .collect::<anyhow::Result<_>>()?,
        Some(_) => anyhow::bail!("buffers is not a list"),
    };
    if let Some(m) = v.as_object_mut() {
        m.remove("channel");
    }
    let mut msg: Message = serde_json::from_value(v)?;
    msg.buffers = buffers;
    Ok(msg)
}

/// A UTF-8 string from C, or None for NULL.
///
/// # Safety
///
/// `s` is NULL or a NUL-terminated string.
unsafe fn from_c<'a>(s: *const c_char) -> Option<std::borrow::Cow<'a, str>> {
    // SAFETY: as the caller promises.
    (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) }.to_string_lossy())
}

/// Lists `spec_json` as a kernelspec whose kernels the app runs through
/// `callbacks`, after the server's other kernels and in place of one of
/// the same name. `spec_json` has `kernel.json`'s keys plus `"name"`; `argv`
/// may be left out. The kernelspec's interrupts go over the control
/// channel, whatever it says.
///
/// Register after [`ganymede_server_start`] and before loading the page,
/// so the first kernel list includes it.
///
/// The server calls `start`, `send` and `stop` on its own threads, never
/// the app's main thread, possibly at once for different kernels; they
/// must not block. Calls to `send` for one kernel don't overlap and come
/// in order. Strings passed to a callback are valid during the call. The
/// server calls `release` once, when it stops, after every other
/// callback.
///
/// Returns 0 on success. On failure (a bad spec, a NULL `start`, `send`
/// or `stop`, a name already registered) returns nonzero, never calls
/// any callback, `release` included, so `ctx` stays the app's, and, if
/// `error_out` is not NULL, sets `*error_out` to a message to free with
/// [`ganymede_string_free`].
///
/// # Safety
///
/// `server` is live; `spec_json` is a NUL-terminated string;
/// `callbacks` points to a valid struct, copied before this returns;
/// `error_out` is NULL or valid for a write.
#[no_mangle]
pub unsafe extern "C" fn ganymede_kernel_register(
    server: *mut GanymedeServer,
    spec_json: *const c_char,
    callbacks: *const GanymedeKernelCallbacks,
    error_out: *mut *mut c_char,
) -> c_int {
    // SAFETY: the caller passes a live server, a string or NULL, and a
    // struct or NULL.
    let result = unsafe { register(&*server, from_c(spec_json), callbacks.as_ref()) };
    match result {
        Ok(()) => 0,
        Err(e) => {
            if !error_out.is_null() {
                // SAFETY: the caller passes a pointer valid for a write.
                unsafe { *error_out = into_c(format!("{e:#}")) };
            }
            1
        }
    }
}

fn register(
    server: &GanymedeServer,
    spec_json: Option<std::borrow::Cow<'_, str>>,
    callbacks: Option<&GanymedeKernelCallbacks>,
) -> anyhow::Result<()> {
    let spec_json = spec_json.ok_or_else(|| anyhow::anyhow!("spec_json is NULL"))?;
    let cb = callbacks.ok_or_else(|| anyhow::anyhow!("callbacks is NULL"))?;
    let (Some(start), Some(send), Some(stop)) = (cb.start, cb.send, cb.stop) else {
        anyhow::bail!("start, send and stop must not be NULL");
    };
    let mut value: serde_json::Value = serde_json::from_str(&spec_json)?;
    let Some(map) = value.as_object_mut() else {
        anyhow::bail!("the kernelspec must be a JSON object");
    };
    let name = match map.remove("name") {
        Some(serde_json::Value::String(n)) => n,
        _ => anyhow::bail!("the kernelspec needs a \"name\""),
    };
    map.entry("argv").or_insert(serde_json::json!([]));
    let mut spec: Kernelspec = serde_json::from_value(value)?;
    spec.name = name;
    let (ctx, release) = (cb.ctx, cb.release);
    server.registered.register(spec, || {
        Arc::new(CKernel {
            ctx,
            start,
            send,
            stop,
            release,
        })
    })?;
    Ok(())
}

/// A message from kernel `kernel_id` on `"shell"`, `"control"`,
/// `"stdin"` or `"iopub"`, as JSON in the websocket's shape with buffers
/// as base64 strings. Takes any thread and doesn't block. Messages for one
/// kernel reach the server in the order they are delivered.
///
/// Returns 0 on success, 1 for a NULL string or a message or channel
/// that can't be read, 2 when the kernel isn't running (it ended, or was
/// never started).
///
/// # Safety
///
/// `server` is live; the strings are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn ganymede_kernel_deliver(
    server: *mut GanymedeServer,
    kernel_id: *const c_char,
    channel: *const c_char,
    msg_json: *const c_char,
) -> c_int {
    // SAFETY: the caller passes a live server and strings.
    let (server, id, channel, json) = unsafe {
        (
            &*server,
            from_c(kernel_id),
            from_c(channel),
            from_c(msg_json),
        )
    };
    let (Some(id), Some(channel), Some(json)) = (id, channel, json) else {
        return 1;
    };
    let channel: Channel = match channel.parse() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(kernel = %id, "the app's kernel sent a message on {channel}: {e}");
            return 1;
        }
    };
    let msg = match parse_message(&json) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(kernel = %id, "a message from the app's kernel can't be read: {e:#}");
            return 1;
        }
    };
    match server.registered.deliver(&id, channel, msg) {
        Ok(()) => 0,
        Err(_) => 2,
    }
}

/// Kernel `kernel_id` ended without the server asking. `reason` is
/// `"lost"` when the platform ended it, as iOS ends a suspended app's web
/// content: the server restarts it, counted against its own limit (3,
/// each within 60 s of the last) and not as a crash, and tells clients
/// `"reason": "lost"` with `status: restarting`; past the limit it leaves
/// the kernel dead, with `"reason": "lost"` on `status: dead`. Anything else,
/// NULL included, is a crash, restarted as the server's restart limit
/// says. Takes any thread; an unknown kernel is ignored.
///
/// # Safety
///
/// `server` is live; `kernel_id` is a NUL-terminated string; `reason` is
/// NULL or one.
#[no_mangle]
pub unsafe extern "C" fn ganymede_kernel_ended(
    server: *mut GanymedeServer,
    kernel_id: *const c_char,
    reason: *const c_char,
) {
    // SAFETY: the caller passes a live server and strings.
    let (server, id, reason) = unsafe { (&*server, from_c(kernel_id), from_c(reason)) };
    let Some(id) = id else { return };
    let why = match reason.as_deref() {
        Some("lost") => Ended::Lost,
        _ => Ended::Exited,
    };
    server.registered.ended(&id, why);
}

fn into_c(s: String) -> *mut c_char {
    CString::new(s.replace('\0', " "))
        .expect("NULs replaced")
        .into_raw()
}

fn config(json: Option<&str>) -> anyhow::Result<Config> {
    let mut value: serde_json::Value = match json {
        Some(j) => serde_json::from_str(j)?,
        None => serde_json::json!({}),
    };
    let Some(map) = value.as_object_mut() else {
        anyhow::bail!("the config must be a JSON object");
    };
    map.entry("port").or_insert(0.into());
    Ok(serde_json::from_value(value)?)
}

/// How a build that can't run kernelspecs starts kernels: it doesn't.
/// Paired with an empty kernelspec list, so the server never asks.
struct NoKernels;

const NO_KERNELS: &str = "this build of ganymede-ffi starts no kernels of its own";

#[async_trait]
impl Provisioner for NoKernels {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Err(ganymede_kernel_host::Error::Unsupported(NO_KERNELS))
    }
}

#[async_trait]
impl TransportFactory for NoKernels {
    async fn connect(
        &self,
        _: &LaunchedKernel,
    ) -> ganymede_kernel_host::Result<ganymede_kernel_host::KernelTransport> {
        Err(ganymede_kernel_host::Error::Unsupported(NO_KERNELS))
    }
}

/// What `run_with` is given for kernels. A build without `zmq` or
/// `subprocess` has no default kernels, so without a gateway it gets
/// [`NoKernels`] rather than failing to start: the app may run its
/// kernels elsewhere, as the iPad app runs them in the web page.
fn startup(cfg: &Config) -> Startup {
    if cfg!(all(feature = "zmq", feature = "subprocess")) || gateway_configured(cfg) {
        return Startup::default();
    }
    tracing::info!("built without zmq or subprocess and no gateway_url: no kernels to list");
    let none = std::sync::Arc::new(NoKernels);
    Startup {
        kernelspecs: Some(std::sync::Arc::new(StaticKernelspecs(Vec::new()))),
        provisioner: Some(none.clone()),
        transports: Some(none),
        ..Default::default()
    }
}

fn start(json: Option<&str>) -> anyhow::Result<GanymedeServer> {
    let cfg = config(json)?;
    let kernels = startup(&cfg);
    let registered = Arc::new(RegisteredKernels::new());
    let for_server = registered.clone();
    let machines = Arc::new(Machines::new());
    let machines_for_server = machines.clone();
    let write_observer = Arc::new(WriteObserver::new());
    let write_observer_for_server = write_observer.clone();
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let (url_tx, url_rx) = mpsc::channel::<anyhow::Result<String>>();
    let thread = std::thread::Builder::new()
        .name("ganymede-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("ganymede")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = url_tx.send(Err(e.into()));
                    return;
                }
            };
            let listening = url_tx.clone();
            let startup = Startup {
                on_listening: Some(Box::new(move |url: &str| {
                    let _ = listening.send(Ok(url.to_string()));
                })),
                stop: Some(stop_rx),
                registered: Some(for_server),
                machines: Some(machines_for_server),
                write_observer: Some(write_observer_for_server),
                ..kernels
            };
            if let Err(e) = rt.block_on(run_with(cfg, startup)) {
                tracing::error!("server stopped: {e:#}");
                // Ignored once the URL went out: nobody is waiting.
                let _ = url_tx.send(Err(e));
            }
        })?;
    match wait_for_url(&url_rx, START_TIMEOUT) {
        Ok(Ok(url)) => Ok(GanymedeServer {
            url: CString::new(url)?,
            registered,
            machines,
            write_observer,
            stop: stop_tx,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(Waited::Exited) => {
            let _ = thread.join();
            anyhow::bail!("the server thread exited before listening")
        }
        Err(Waited::TimedOut) => {
            // The thread is stuck before it listens, so it can't be
            // joined. The stop message waits in the channel: should the
            // server listen after all, it stops at once.
            let _ = stop_tx.try_send(());
            tracing::error!("the server did not listen within {START_TIMEOUT:?}");
            anyhow::bail!(
                "the server did not start listening within {} seconds",
                START_TIMEOUT.as_secs()
            )
        }
    }
}

/// How long [`ganymede_server_start`] waits for the server to listen.
/// It reads the root dir, opens its databases and binds a port first,
/// which takes well under a second unless something is stuck.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Why no URL came.
#[derive(Debug, PartialEq)]
enum Waited {
    Exited,
    TimedOut,
}

/// The server thread's first report, within `timeout`.
fn wait_for_url<T>(rx: &mpsc::Receiver<T>, timeout: Duration) -> Result<T, Waited> {
    rx.recv_timeout(timeout).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => Waited::TimedOut,
        mpsc::RecvTimeoutError::Disconnected => Waited::Exited,
    })
}

#[cfg(test)]
mod tests {
    use super::{config, ganymede_set_panic_callback, startup, wait_for_url, Waited};
    use std::ffi::{c_char, CStr};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    static PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn record(message: *const c_char) {
        // SAFETY: the hook passes a NUL-terminated string.
        let message = unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned();
        PANICS.lock().unwrap().push(message);
    }

    #[test]
    fn panic_callback_gets_the_message() {
        ganymede_set_panic_callback(Some(record));
        let _ = std::thread::spawn(|| panic!("test panic {}", 42)).join();
        ganymede_set_panic_callback(None);
        let _ = std::thread::spawn(|| panic!("after removing")).join();
        // Other tests' panics (v8_bench's) may land while the callback is
        // installed: look only at this test's own.
        let panics: Vec<String> = PANICS
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("test panic") || p.contains("after removing"))
            .cloned()
            .collect();
        assert_eq!(panics.len(), 1, "{panics:?}");
        assert!(
            panics[0].starts_with("panicked at crates/ganymede-ffi/src/lib.rs:"),
            "{}",
            panics[0]
        );
        assert!(panics[0].ends_with(": test panic 42"), "{}", panics[0]);
    }

    #[test]
    fn start_gives_up() {
        let (tx, rx) = mpsc::channel::<()>();
        let short = Duration::from_millis(10);
        assert_eq!(wait_for_url(&rx, short), Err(Waited::TimedOut));
        tx.send(()).unwrap();
        assert_eq!(wait_for_url(&rx, short), Ok(()));
        drop(tx);
        assert_eq!(wait_for_url(&rx, short), Err(Waited::Exited));
    }

    #[test]
    fn port_defaults_to_any() {
        assert_eq!(config(None).unwrap().port, 0);
        assert_eq!(config(Some(r#"{"port": 9999}"#)).unwrap().port, 9999);
        assert_eq!(config(Some("{}")).unwrap().ip, "127.0.0.1");
    }

    #[test]
    fn static_headers_come_through_the_json_config() {
        let cfg = config(Some(
            r#"{"static_headers": {"/kernel.html": {"Cross-Origin-Opener-Policy": "same-origin"}}}"#,
        ))
        .unwrap();
        assert_eq!(
            cfg.static_headers["/kernel.html"]["Cross-Origin-Opener-Policy"],
            "same-origin"
        );
    }

    #[test]
    fn bad_config_is_an_error() {
        assert!(config(Some("[]")).is_err());
        assert!(config(Some(r#"{"no_such_key": 1}"#)).is_err());
    }

    #[test]
    fn kernels_without_processes() {
        let local = startup(&config(None).unwrap());
        let remote = startup(&config(Some(r#"{"gateway_url": "http://k:8888"}"#)).unwrap());
        assert!(remote.kernelspecs.is_none() && remote.provisioner.is_none());
        let full = cfg!(all(feature = "zmq", feature = "subprocess"));
        assert_eq!(local.kernelspecs.is_none(), full);
        assert_eq!(local.provisioner.is_none(), full);
        assert_eq!(local.transports.is_none(), full);
        if let Some(specs) = local.kernelspecs {
            assert!(specs.list().unwrap().is_empty());
        }
    }
}
