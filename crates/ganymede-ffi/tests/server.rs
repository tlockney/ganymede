//! The C ABI, called as an app calls it.

use std::ffi::{c_char, CStr, CString};
use std::io::{Read, Write};
use std::net::TcpStream;

use ganymede_ffi::{
    ganymede_server_start, ganymede_server_stop, ganymede_server_url, ganymede_string_free,
};

/// `GET path` with the token as a header; the status line and body.
fn get(url: &str, path: &str) -> String {
    let rest = url.strip_prefix("http://").unwrap();
    let (addr, query) = rest.split_once('/').unwrap();
    let token = query.strip_prefix("?token=").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: token {token}\r\nConnection: close\r\n\r\n"
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
