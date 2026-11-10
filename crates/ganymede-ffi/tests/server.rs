//! The C ABI, called as an app calls it.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use ganymede_ffi::{
    ganymede_kernel_deliver, ganymede_kernel_ended, ganymede_kernel_register,
    ganymede_server_start, ganymede_server_stop, ganymede_server_url, ganymede_string_free,
    GanymedeKernelCallbacks, GanymedeServer,
};

/// `GET path` with the token as a header; the status line and body.
fn get(url: &str, path: &str) -> String {
    request(url, "GET", path, "")
}

/// `method path` with `body` as JSON and the token as a header; the
/// status line and body.
fn request(url: &str, method: &str, path: &str, body: &str) -> String {
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, query) = rest.split_once('/').unwrap();
    let token = query.strip_prefix("?token=").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: token {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn starts_serves_and_stops() {
    let root = tempfile::tempdir().unwrap();
    let cfg = CString::new(format!(
        r#"{{"root_dir": {:?}, "cookie_secret_file": {:?}}}"#,
        root.path(),
        root.path().join("secret")
    ))
    .unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let server = unsafe { ganymede_server_start(cfg.as_ptr(), &mut err) };
    assert!(err.is_null());
    assert!(!server.is_null());
    let url = unsafe { CStr::from_ptr(ganymede_server_url(server)) }
        .to_str()
        .unwrap()
        .to_string();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let reply = get(&url, "/api/status");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    unsafe { ganymede_server_stop(server) };
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();
    assert!(
        TcpStream::connect(addr).is_err(),
        "still listening after stop"
    );
}

#[test]
fn failure_returns_null_and_a_message() {
    let cfg = CString::new(r#"{"root_dir": "/no/such/dir"}"#).unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let server = unsafe { ganymede_server_start(cfg.as_ptr(), &mut err) };
    assert!(server.is_null());
    let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
    assert!(msg.contains("/no/such/dir"), "{msg}");
    unsafe { ganymede_string_free(err) };
    // A null error_out is allowed too.
    assert!(unsafe { ganymede_server_start(cfg.as_ptr(), std::ptr::null_mut()) }.is_null());
}

/// Built as for iOS (`--no-default-features --features gateway`), the
/// server starts with no kernels instead of failing.
#[cfg(not(all(feature = "zmq", feature = "subprocess")))]
#[test]
fn starts_with_no_kernels_without_processes() {
    let root = tempfile::tempdir().unwrap();
    let cfg = CString::new(format!(
        r#"{{"root_dir": {:?}, "cookie_secret_file": {:?}}}"#,
        root.path(),
        root.path().join("secret")
    ))
    .unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let server = unsafe { ganymede_server_start(cfg.as_ptr(), &mut err) };
    assert!(!server.is_null(), "{}", unsafe {
        CStr::from_ptr(err).to_string_lossy()
    });
    let url = unsafe { CStr::from_ptr(ganymede_server_url(server)) }
        .to_str()
        .unwrap()
        .to_string();
    let reply = get(&url, "/api/kernelspecs");
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.contains(r#""kernelspecs":{}"#), "{reply}");
    unsafe { ganymede_server_stop(server) };
}

/// What the fake app's callbacks saw.
#[derive(Default)]
struct App {
    server: AtomicPtr<GanymedeServer>,
    starts: Mutex<Vec<serde_json::Value>>,
    stops: AtomicUsize,
    released: AtomicUsize,
}

fn str_of<'a>(s: *const c_char) -> &'a str {
    unsafe { CStr::from_ptr(s) }.to_str().unwrap()
}

extern "C" fn start(ctx: *mut c_void, _id: *const c_char, launch: *const c_char) -> c_int {
    let app = unsafe { &*(ctx as *const App) };
    app.starts
        .lock()
        .unwrap()
        .push(serde_json::from_str(str_of(launch)).unwrap());
    0
}

/// Answers kernel_info and shutdown requests as a kernel would, through
/// `ganymede_kernel_deliver`, from inside the callback.
extern "C" fn send(
    ctx: *mut c_void,
    id: *const c_char,
    channel: *const c_char,
    msg: *const c_char,
) {
    let app = unsafe { &*(ctx as *const App) };
    let msg: serde_json::Value = serde_json::from_str(str_of(msg)).unwrap();
    let reply_type = match msg["header"]["msg_type"].as_str().unwrap() {
        "kernel_info_request" => "kernel_info_reply",
        "shutdown_request" => "shutdown_reply",
        _ => return,
    };
    let header = |msg_type: &str| {
        serde_json::json!({
            "msg_id": format!("{msg_type}-{}", msg["header"]["msg_id"].as_str().unwrap()),
            "msg_type": msg_type,
            "session": "fake",
            "username": "fake",
            "date": "2026-11-10T00:00:00Z",
            "version": "5.3",
        })
    };
    let status = |state: &str| {
        serde_json::json!({
            "header": header("status"),
            "parent_header": msg["header"],
            "metadata": {},
            "content": {"execution_state": state},
        })
    };
    let content = if reply_type == "kernel_info_reply" {
        serde_json::json!({
            "status": "ok",
            "protocol_version": "5.3",
            "implementation": "fake",
            "implementation_version": "1",
            "language_info": {"name": "typescript", "version": "5", "file_extension": ".ts"},
            "banner": "",
            "capabilities": {"device": true},
        })
    } else {
        serde_json::json!({"status": "ok", "restart": false})
    };
    let reply = serde_json::json!({
        "header": header(reply_type),
        "parent_header": msg["header"],
        "metadata": {},
        "content": content,
        "buffers": ["AAEC"],
    });
    let server = app.server.load(Ordering::SeqCst);
    let id = unsafe { CStr::from_ptr(id) };
    for (channel, m) in [
        (c"iopub", status("busy")),
        (unsafe { CStr::from_ptr(channel) }, reply),
        (c"iopub", status("idle")),
    ] {
        // Nonzero when the server has stopped the kernel meanwhile, which
        // an app has to accept; the test checks what arrived instead.
        let m = CString::new(m.to_string()).unwrap();
        unsafe { ganymede_kernel_deliver(server, id.as_ptr(), channel.as_ptr(), m.as_ptr()) };
    }
}

extern "C" fn stop(ctx: *mut c_void, _id: *const c_char) {
    let app = unsafe { &*(ctx as *const App) };
    app.stops.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn release(ctx: *mut c_void) {
    let app = unsafe { &*(ctx as *const App) };
    app.released.fetch_add(1, Ordering::SeqCst);
}

/// Waits up to five seconds for `ok`.
fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..100 {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn runs_a_kernel_the_app_registers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.ipynb"), "").unwrap();
    let cfg = CString::new(format!(
        r#"{{"root_dir": {:?}, "cookie_secret_file": {:?}, "kernel_restart_limit": 1}}"#,
        root.path(),
        root.path().join("secret")
    ))
    .unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let server = unsafe { ganymede_server_start(cfg.as_ptr(), &mut err) };
    assert!(!server.is_null());
    let url = str_of(unsafe { ganymede_server_url(server) }).to_string();

    let app = Box::leak(Box::<App>::default());
    app.server.store(server, Ordering::SeqCst);
    let callbacks = GanymedeKernelCallbacks {
        ctx: app as *mut App as *mut c_void,
        start: Some(start),
        send: Some(send),
        stop: Some(stop),
        release: Some(release),
    };
    let spec =
        CString::new(r#"{"name": "app-ts", "display_name": "TypeScript (app)", "language": "typescript", "metadata": {"capabilities": {"device": true}}}"#)
            .unwrap();
    assert_eq!(
        unsafe { ganymede_kernel_register(server, spec.as_ptr(), &callbacks, &mut err) },
        0
    );
    // The same name again fails, with a message, and never releases.
    assert_ne!(
        unsafe { ganymede_kernel_register(server, spec.as_ptr(), &callbacks, &mut err) },
        0
    );
    assert!(
        str_of(err).contains("already registered"),
        "{}",
        str_of(err)
    );
    unsafe { ganymede_string_free(err) };

    let specs = get(&url, "/api/kernelspecs");
    assert!(specs.contains(r#""app-ts""#), "{specs}");
    assert!(specs.contains(r#""interrupt_mode":"message""#), "{specs}");

    // A session starts the kernel with the notebook's path.
    let reply = request(
        &url,
        "POST",
        "/api/sessions",
        r#"{"path": "a.ipynb", "type": "notebook", "name": "a.ipynb", "kernel": {"name": "app-ts"}}"#,
    );
    assert!(reply.starts_with("HTTP/1.1 201"), "{reply}");
    let body: serde_json::Value =
        serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let id = body["kernel"]["id"].as_str().unwrap().to_string();
    {
        let starts = app.starts.lock().unwrap();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0]["kernelspec"], "app-ts");
        let notebook = starts[0]["env"]["JPY_SESSION_NAME"].as_str().unwrap();
        assert!(notebook.ends_with("a.ipynb"), "{notebook}");
    }

    // Started once it answers; until then a death is a failed start.
    eventually("the kernel", || {
        get(&url, &format!("/api/kernels/{id}")).contains(r#""execution_state":"idle""#)
    });

    // The platform ends it: the server starts it again, under the same id.
    let cid = CString::new(id.clone()).unwrap();
    unsafe { ganymede_kernel_ended(server, cid.as_ptr(), c"lost".as_ptr()) };
    eventually("the restart", || app.starts.lock().unwrap().len() == 2);
    eventually("the restarted kernel", || {
        get(&url, &format!("/api/kernels/{id}")).contains(r#""execution_state":"idle""#)
    });
    // Twice within ten seconds, with a restart limit of one: a crash
    // would now be left dead, but a loss doesn't count.
    unsafe { ganymede_kernel_ended(server, cid.as_ptr(), c"lost".as_ptr()) };
    eventually("the second restart", || {
        app.starts.lock().unwrap().len() == 3
    });
    eventually("the kernel again", || {
        get(&url, &format!("/api/kernels/{id}")).contains(r#""execution_state":"idle""#)
    });
    assert_eq!(app.stops.load(Ordering::SeqCst), 0);

    // A shutdown asks the kernel, then stops it.
    let reply = request(&url, "DELETE", &format!("/api/kernels/{id}"), "");
    assert!(reply.starts_with("HTTP/1.1 204"), "{reply}");
    assert_eq!(app.stops.load(Ordering::SeqCst), 1);
    assert_ne!(
        unsafe { ganymede_kernel_deliver(server, cid.as_ptr(), c"iopub".as_ptr(), c"{}".as_ptr()) },
        0
    );

    assert_eq!(app.released.load(Ordering::SeqCst), 0);
    unsafe { ganymede_server_stop(server) };
    assert_eq!(app.released.load(Ordering::SeqCst), 1);
}
