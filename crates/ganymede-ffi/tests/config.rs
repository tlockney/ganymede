//! The C ABI's config file functions, called as an app calls them.

use std::ffi::{c_char, CStr, CString};

use ganymede_ffi::{ganymede_config_read, ganymede_config_set, ganymede_string_free};

/// The string a call returned, freed. A NULL string is a test failure.
fn take(s: *mut c_char) -> String {
    assert!(!s.is_null(), "no string");
    // SAFETY: the library returned a NUL-terminated string, and it is not
    // used after this.
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    // SAFETY: it came from the library and is not used again.
    unsafe { ganymede_string_free(s) };
    text
}

/// `text` as a C string.
fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

#[test]
fn set_then_read_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ganymede.toml");
    std::fs::write(&path, "# A comment stays.\nport = 8888\n").unwrap();
    let dir = c(dir.path().to_str().unwrap());
    let patch = c(r#"{"mcp_enabled": true}"#);

    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: all three are strings or NULL.
    let code = unsafe { ganymede_config_set(dir.as_ptr(), patch.as_ptr(), &mut err) };
    assert_eq!(code, 0);
    assert!(err.is_null());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# A comment stays.\nport = 8888\nmcp_enabled = true\n"
    );

    // SAFETY: as above; the string is freed by `take`.
    let keys = unsafe { ganymede_config_read(dir.as_ptr(), &mut err) };
    assert!(err.is_null(), "{err:?}");
    let keys: serde_json::Value = serde_json::from_str(&take(keys)).unwrap();
    assert_eq!(keys, serde_json::json!({"port": 8888, "mcp_enabled": true}));
}

#[test]
fn a_refused_set_says_why_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ganymede.toml");
    std::fs::write(&path, "port = 8888\n").unwrap();
    let before = std::fs::read(&path).unwrap();
    let dir = c(dir.path().to_str().unwrap());
    let patch = c(r#"{"no_such_key": 1}"#);

    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: all three are strings or NULL.
    let code = unsafe { ganymede_config_set(dir.as_ptr(), patch.as_ptr(), &mut err) };
    assert_ne!(code, 0);
    let message = take(err);
    assert!(message.contains("unknown field `no_such_key`"), "{message}");
    assert!(message.contains("ganymede.toml"), "{message}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_null_json_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let dir = c(dir.path().to_str().unwrap());
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: dir is a string and json is NULL, which the contract allows.
    let code = unsafe { ganymede_config_set(dir.as_ptr(), std::ptr::null(), &mut err) };
    assert_ne!(code, 0);
    assert_eq!(take(err), "json is NULL");
}

#[test]
fn json_that_is_not_an_object_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let dir = c(dir.path().to_str().unwrap());
    let patch = c("[1]");
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: all three are strings or NULL.
    let code = unsafe { ganymede_config_set(dir.as_ptr(), patch.as_ptr(), &mut err) };
    assert_ne!(code, 0);
    assert_eq!(take(err), "expected a JSON object");
}

#[test]
fn a_missing_file_reads_as_an_empty_object() {
    let dir = tempfile::tempdir().unwrap();
    let missing = c(dir.path().join("missing").to_str().unwrap());
    // A NULL error_out is allowed too.
    // SAFETY: dir is a string; error_out is NULL.
    let keys = unsafe { ganymede_config_read(missing.as_ptr(), std::ptr::null_mut()) };
    assert_eq!(take(keys), "{}");
}

#[test]
fn a_file_that_is_not_toml_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ganymede.toml");
    std::fs::write(&path, "port = ").unwrap();
    let dir = c(dir.path().to_str().unwrap());
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: dir is a string; error_out is valid for a write.
    let keys = unsafe { ganymede_config_read(dir.as_ptr(), &mut err) };
    assert!(keys.is_null());
    let message = take(err);
    assert!(message.contains("ganymede.toml"), "{message}");
    assert!(message.contains("TOML parse error"), "{message}");
}
