//! `ganymede_config_read` and `ganymede_config_set` with a NULL dir: the
//! default config dir, and the legacy file in the Jupyter config dir that
//! `ganymede serve` reads in its place. Its own test binary with one test,
//! since it sets the process's environment.

use std::ffi::{c_char, CStr, CString};

use ganymede_ffi::{ganymede_config_read, ganymede_config_set, ganymede_string_free};

/// The string a call returned, freed.
fn take(s: *mut c_char) -> String {
    assert!(!s.is_null(), "no string");
    // SAFETY: the library returned a NUL-terminated string, not used after.
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    // SAFETY: it came from the library and is not used again.
    unsafe { ganymede_string_free(s) };
    text
}

#[test]
fn a_null_dir_follows_the_file_the_server_reads() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let jupyter = root.path().join("jupyter");
    std::fs::create_dir_all(&jupyter).unwrap();
    std::env::set_var("GANYMEDE_CONFIG_DIR", &config);
    std::env::set_var("JUPYTER_CONFIG_DIR", &jupyter);
    let legacy = jupyter.join("ganymede.toml");
    std::fs::write(&legacy, "port = 9999\n").unwrap();
    let patch = CString::new(r#"{"mcp_enabled": true}"#).unwrap();

    // Read gives the legacy file, which the server reads.
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: a NULL dir and a pointer valid for a write.
    let json = take(unsafe { ganymede_config_read(std::ptr::null(), &mut err) });
    assert!(err.is_null());
    assert_eq!(json, r#"{"port":9999}"#);

    // Set refuses, and creates nothing.
    // SAFETY: a NULL dir, a string, and a pointer valid for a write.
    let code = unsafe { ganymede_config_set(std::ptr::null(), patch.as_ptr(), &mut err) };
    assert_ne!(code, 0);
    assert!(take(err).contains("is in the Jupyter config dir"));
    assert!(!config.exists());

    // Once the file is moved, set writes it in the config dir.
    std::fs::create_dir_all(&config).unwrap();
    std::fs::rename(&legacy, config.join("ganymede.toml")).unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: as above.
    let code = unsafe { ganymede_config_set(std::ptr::null(), patch.as_ptr(), &mut err) };
    assert_eq!(
        code,
        0,
        "{}",
        if err.is_null() {
            String::new()
        } else {
            take(err)
        }
    );
    assert_eq!(
        std::fs::read_to_string(config.join("ganymede.toml")).unwrap(),
        "port = 9999\nmcp_enabled = true\n"
    );
}
