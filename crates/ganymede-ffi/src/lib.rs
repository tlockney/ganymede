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
use std::sync::mpsc;
use std::thread::JoinHandle;

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
    match url_rx.recv() {
        Ok(Ok(url)) => Ok(GanymedeServer {
            url: CString::new(url)?,
            stop: stop_tx,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            anyhow::bail!("the server thread exited before listening")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{config, startup};

    #[test]
    fn port_defaults_to_any() {
        assert_eq!(config(None).unwrap().port, 0);
        assert_eq!(config(Some(r#"{"port": 9999}"#)).unwrap().port, 9999);
        assert_eq!(config(Some("{}")).unwrap().ip, "127.0.0.1");
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
