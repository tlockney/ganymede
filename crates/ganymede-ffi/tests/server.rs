//! The C ABI, called as an app calls it.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use ganymede_ffi::{
    ganymede_kernel_deliver, ganymede_kernel_ended, ganymede_kernel_register,
    ganymede_server_add_machine, ganymede_server_machines, ganymede_server_remove_machine,
    ganymede_server_set_write_observer, ganymede_server_start, ganymede_server_stop,
    ganymede_server_url, ganymede_string_free, GanymedeKernelCallbacks, GanymedeServer,
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

/// A well-formed iopub status message.
const STATUS: &std::ffi::CStr = c"{\"header\": {\"msg_id\": \"m\", \"msg_type\": \"status\", \"username\": \"\", \"session\": \"s\", \"date\": \"2026-01-01T00:00:00Z\", \"version\": \"5.3\"}, \"parent_header\": {}, \"metadata\": {}, \"content\": {\"execution_state\": \"idle\"}}";

/// The Jupyter runtime dir for this test run's servers: a temp dir, set
/// once before any of them starts, so their rooms lock there and not in
/// the real one (`~/Library/Jupyter/runtime`). The servers run in this
/// process, so the var can't be set per server as the CLI tests set it
/// per command; set once and never changed, parallel tests can't race
/// it. Kept, not deleted: the locks outlive the call that set it.
fn temp_runtime_dir() {
    static RUNTIME_DIR: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    RUNTIME_DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("a temp runtime dir");
        std::env::set_var("JUPYTER_RUNTIME_DIR", dir.keep());
    });
}

#[test]
fn starts_serves_and_stops() {
    temp_runtime_dir();
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
    temp_runtime_dir();
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
    temp_runtime_dir();
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
    temp_runtime_dir();
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
    // It has no directory, so no files: not even ones in the working
    // directory.
    let file = get(&url, "/kernelspecs/app-ts/Cargo.toml");
    assert!(file.starts_with("HTTP/1.1 404"), "{file}");

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
    // would now be left dead, but a loss has its own limit.
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
    // A message for it now has no kernel to go to.
    assert_eq!(
        unsafe {
            ganymede_kernel_deliver(server, cid.as_ptr(), c"iopub".as_ptr(), STATUS.as_ptr())
        },
        2
    );

    assert_eq!(app.released.load(Ordering::SeqCst), 0);
    unsafe { ganymede_server_stop(server) };
    assert_eq!(app.released.load(Ordering::SeqCst), 1);
}

/// With the default limits, three losses in a row restart the kernel and
/// the fourth leaves it dead (#844).
#[test]
fn a_kernel_the_platform_keeps_ending_is_left_dead() {
    temp_runtime_dir();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.ipynb"), "").unwrap();
    let cfg = CString::new(format!(
        r#"{{"root_dir": {:?}, "cookie_secret_file": {:?}}}"#,
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
    let spec = CString::new(
        r#"{"name": "app-ts", "display_name": "TypeScript (app)", "language": "typescript"}"#,
    )
    .unwrap();
    assert_eq!(
        unsafe { ganymede_kernel_register(server, spec.as_ptr(), &callbacks, &mut err) },
        0
    );
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
    let cid = CString::new(id.clone()).unwrap();
    let idle =
        |url: &str| get(url, &format!("/api/kernels/{id}")).contains(r#""execution_state":"idle""#);
    eventually("the kernel", || idle(&url));

    for n in 2..=4 {
        unsafe { ganymede_kernel_ended(server, cid.as_ptr(), c"lost".as_ptr()) };
        eventually("the restart", || app.starts.lock().unwrap().len() == n);
        eventually("the restarted kernel", || idle(&url));
    }
    // The fourth loss in a row is left dead.
    unsafe { ganymede_kernel_ended(server, cid.as_ptr(), c"lost".as_ptr()) };
    eventually("the kernel to be removed", || {
        get(&url, &format!("/api/kernels/{id}")).starts_with("HTTP/1.1 404")
    });
    assert_eq!(app.starts.lock().unwrap().len(), 4);

    unsafe { ganymede_server_stop(server) };
}

/// A server started through the C ABI, its URL and its token.
fn start_server(root: &std::path::Path, name: &str) -> (*mut GanymedeServer, String) {
    temp_runtime_dir();
    let cfg = CString::new(format!(
        r#"{{"root_dir": {:?}, "cookie_secret_file": {:?}, "notary_dir": {:?}}}"#,
        root,
        root.join(format!("{name}-secret")),
        root.join(format!("{name}-notary")),
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
    (server, url)
}

/// The status code in a raw reply.
fn code(reply: &str) -> &str {
    &reply[9..12]
}

/// The ids `ganymede_server_machines` lists, freeing its string.
fn machines(server: *mut GanymedeServer) -> Vec<String> {
    let list = unsafe { ganymede_server_machines(server) };
    assert!(!list.is_null());
    let json = unsafe { CStr::from_ptr(list) }
        .to_str()
        .unwrap()
        .to_string();
    unsafe { ganymede_string_free(list) };
    serde_json::from_str(&json).unwrap()
}

#[test]
fn mounts_and_unmounts_a_machine() {
    let root = tempfile::tempdir().unwrap();
    let mini_dir = root.path().join("mini");
    std::fs::create_dir(&mini_dir).unwrap();
    std::fs::write(mini_dir.join("hello.txt"), "from the mini").unwrap();
    let local_dir = root.path().join("local");
    std::fs::create_dir(&local_dir).unwrap();
    let (mini, mini_url) = start_server(&mini_dir, "mini");
    let (local, local_url) = start_server(&local_dir, "local");
    let (mini_base, mini_token) = mini_url.split_once("?token=").unwrap();
    let c = |s: &str| CString::new(s).unwrap();
    let (id, base, token) = (c("mini"), c(mini_base), c(mini_token));
    let mut err: *mut c_char = std::ptr::null_mut();

    // Nothing is mounted until the app adds it.
    let path = "/machines/mini/api/contents/hello.txt";
    assert_eq!(code(&get(&local_url, path)), "404");
    assert!(machines(local).is_empty());
    assert!(unsafe { ganymede_server_machines(std::ptr::null()) }.is_null());

    let added = unsafe {
        ganymede_server_add_machine(local, id.as_ptr(), base.as_ptr(), token.as_ptr(), &mut err)
    };
    assert_eq!(added, 0);
    let reply = get(&local_url, path);
    assert_eq!(code(&reply), "200", "{reply}");
    assert!(reply.contains("from the mini"), "{reply}");
    // The machine's token isn't what the page sees, and the local server
    // doesn't have the file.
    assert!(!reply.contains(mini_token), "{reply}");
    assert_eq!(code(&get(&local_url, "/api/contents/hello.txt")), "404");

    // Without the token a machine that wants one refuses, and the answer
    // is the machine's own 403.
    let bare = c("bare");
    let added = unsafe {
        ganymede_server_add_machine(
            local,
            bare.as_ptr(),
            base.as_ptr(),
            std::ptr::null(),
            &mut err,
        )
    };
    assert_eq!(added, 0);
    assert_eq!(code(&get(&local_url, "/machines/bare/api/status")), "403");
    // A bad id or URL is refused with a message, and NULL is not a crash.
    for (id, url) in [("a/b", "http://h/"), ("", "http://h/"), ("ok", "ftp://h/")] {
        let (id, url) = (c(id), c(url));
        let added = unsafe {
            ganymede_server_add_machine(
                local,
                id.as_ptr(),
                url.as_ptr(),
                std::ptr::null(),
                &mut err,
            )
        };
        assert_ne!(added, 0);
        let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        assert!(!msg.is_empty());
        unsafe { ganymede_string_free(err) };
        err = std::ptr::null_mut();
    }
    let added = unsafe {
        ganymede_server_add_machine(
            local,
            std::ptr::null(),
            base.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    assert_ne!(added, 0);
    assert_eq!(
        unsafe { ganymede_server_remove_machine(local, std::ptr::null()) },
        0
    );

    // The mounted ones are listed, sorted, and the refused adds are not.
    // `a` is added last and sorts first, so the order is the sort's.
    let a = c("a");
    let added = unsafe {
        ganymede_server_add_machine(
            local,
            a.as_ptr(),
            base.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(added, 0);
    assert_eq!(machines(local), ["a", "bare", "mini"]);
    assert_eq!(
        unsafe { ganymede_server_remove_machine(local, a.as_ptr()) },
        1
    );

    // Removing says whether it was mounted, and unmounts it.
    assert_eq!(
        unsafe { ganymede_server_remove_machine(local, id.as_ptr()) },
        1
    );
    assert_eq!(
        unsafe { ganymede_server_remove_machine(local, id.as_ptr()) },
        0
    );
    assert_eq!(code(&get(&local_url, path)), "404");
    // The other machine is still mounted.
    assert_eq!(code(&get(&local_url, "/machines/bare/api/status")), "403");
    assert_eq!(machines(local), ["bare"]);

    unsafe { ganymede_server_stop(local) };
    unsafe { ganymede_server_stop(mini) };
}

/// What the write callback was given.
type Wrote = Vec<(String, Option<String>, Option<String>)>;

extern "C" fn record_write(
    ctx: *mut c_void,
    path: *const c_char,
    before: *const c_char,
    after: *const c_char,
) {
    let opt = |s: *const c_char| {
        (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) }.to_str().unwrap().to_string())
    };
    // SAFETY: the test passes a `WriteCtx` that outlives the server.
    let wrote = &unsafe { &*(ctx as *const WriteCtx) }.wrote;
    let path = opt(path).expect("a path");
    wrote.lock().unwrap().push((path, opt(before), opt(after)));
}

/// The test's context for the write callback: what it was given, and how
/// many times it was released.
#[derive(Default)]
struct WriteCtx {
    wrote: Mutex<Wrote>,
    released: std::sync::atomic::AtomicUsize,
}

extern "C" fn release_write_ctx(ctx: *mut c_void) {
    // SAFETY: the test passes a `WriteCtx` that outlives the server.
    let ctx = unsafe { &*(ctx as *const WriteCtx) };
    ctx.released
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// `PUT` of text to a file through REST: a write the server makes.
fn put_text(url: &str, name: &str, text: &str) {
    let body = format!(r#"{{"type": "file", "format": "text", "content": {text:?}}}"#);
    let reply = request(url, "PUT", &format!("/api/contents/{name}"), &body);
    assert!(code(&reply) == "200" || code(&reply) == "201", "{reply}");
}

#[test]
fn reports_the_servers_writes_to_the_app() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().canonicalize().unwrap();
    let (server, url) = start_server(&dir, "observed");
    let (first, second) = (WriteCtx::default(), WriteCtx::default());
    let ctx = |m: &WriteCtx| m as *const WriteCtx as *mut c_void;
    let released = |m: &WriteCtx| m.released.load(std::sync::atomic::Ordering::SeqCst);
    let set = |server, c: &WriteCtx| unsafe {
        ganymede_server_set_write_observer(
            server,
            Some(record_write),
            Some(release_write_ctx),
            ctx(c),
        )
    };

    // Nothing is set: a write is not reported, and goes on.
    put_text(&url, "a.txt", "none");

    set(server, &first);
    put_text(&url, "a.txt", "one");
    put_text(&url, "new.txt", "made");
    {
        let calls = first.wrote.lock().unwrap();
        assert_eq!(calls.len(), 2, "{calls:?}");
        let path = dir.join("a.txt").to_str().unwrap().to_string();
        assert_eq!(calls[0].0, path);
        let before = calls[0].1.as_deref().expect("the file was there");
        // size:mtime ns:inode, the stamp of a four-byte file, then three.
        assert!(before.starts_with("4:"), "{before}");
        let after = calls[0].2.as_deref().expect("a stamp after");
        assert!(after.starts_with("3:") && after != before, "{after}");
        assert_eq!(calls[1].0, dir.join("new.txt").to_str().unwrap());
        assert_eq!(calls[1].1, None, "a new file has no stamp before");
        assert!(calls[1].2.is_some());
    }
    // The next write starts from the stamp the last one left.
    put_text(&url, "a.txt", "three");
    let after_one = first.wrote.lock().unwrap()[0].2.clone();
    assert_eq!(first.wrote.lock().unwrap()[2].1, after_one);

    // Replaced: the later writes go to the new callback and context.
    set(server, &second);
    put_text(&url, "a.txt", "four");
    assert_eq!(first.wrote.lock().unwrap().len(), 3);
    assert_eq!(second.wrote.lock().unwrap().len(), 1);
    assert_eq!((released(&first), released(&second)), (1, 0), "replaced");

    // Removed.
    unsafe { ganymede_server_set_write_observer(server, None, None, std::ptr::null_mut()) };
    put_text(&url, "a.txt", "five");
    assert_eq!(first.wrote.lock().unwrap().len(), 3);
    assert_eq!(second.wrote.lock().unwrap().len(), 1);
    assert_eq!((released(&first), released(&second)), (1, 1), "removed");

    // A write that fails is not reported.
    set(server, &second);
    let bad = request(
        &url,
        "PUT",
        "/api/contents/no-such-dir/x.txt",
        r#"{"type": "file", "format": "text", "content": "x"}"#,
    );
    assert!(!code(&bad).starts_with('2'), "{bad}");
    assert_eq!(second.wrote.lock().unwrap().len(), 1);

    // Stopping the server releases the one still set, once.
    assert_eq!(released(&second), 1);
    unsafe { ganymede_server_stop(server) };
    assert_eq!((released(&first), released(&second)), (1, 2), "stopped");
}
