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
//! Built without the `zmq` or `subprocess` feature, as the iOS slices
//! of the XCFramework are, the server can't run kernelspecs itself. It
//! then lists no kernels and refuses to start one, unless the config
//! sets `gateway_url`, whose kernels it runs as usual.

use std::ffi::{c_char, CStr, CString};
use std::sync::{mpsc, Once, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use async_trait::async_trait;
use ganymede_api::StaticKernelspecs;
use ganymede_kernel_host::{
    Kernelspec, LaunchOptions, LaunchedKernel, Provisioner, TransportFactory,
};
use ganymede_server::config::Config;
use ganymede_server::serve::{gateway_configured, run_with, Startup};

/// A running server. Opaque to C.
pub struct GanymedeServer {
    url: CString,
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
        let panics = PANICS.lock().unwrap();
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
