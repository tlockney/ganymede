//! Shared by the end-to-end tests.
//!
//! Real kernels and the Jupyter reference implementation are optional so
//! `cargo test` passes on a bare machine. Tests that need them skip with a
//! note, unless the environment requires them, which CI does:
//! - `GANYMEDE_E2E_KERNELS=cdp-node,python3`: these kernelspecs must exist.
//! - `GANYMEDE_E2E_JUPYTER=<path to jupyter>`: the reference to compare
//!   against (`jupyter execute`, `jupyter server`). Found on `PATH`
//!   otherwise. `GANYMEDE_E2E_REQUIRE_JUPYTER=1` makes it mandatory.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ganymede"))
}

pub fn ganymede(args: &[&str]) -> Command {
    let mut c = Command::new(bin());
    c.args(args);
    c
}

pub fn run_ok(mut cmd: impl std::borrow::BorrowMut<Command>) -> Output {
    let cmd = cmd.borrow_mut();
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The repository root.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn installed() -> Vec<String> {
    let out = ganymede(&["kernelspec", "list", "--json"])
        .output()
        .expect("spawn");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    v["kernelspecs"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn required() -> Vec<String> {
    std::env::var("GANYMEDE_E2E_KERNELS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// True when the kernelspec is installed. Panics when it is required and
/// missing; otherwise prints why the caller skips.
pub fn have_kernel(name: &str) -> bool {
    if installed().iter().any(|k| k == name) {
        return true;
    }
    assert!(
        !required().iter().any(|k| k == name),
        "kernelspec {name} is required by GANYMEDE_E2E_KERNELS but not installed"
    );
    eprintln!("skipping: kernelspec {name} not installed");
    false
}

/// The kernels the cross-kernel scenarios run on.
pub const KERNELS: &[&str] = &["cdp-node", "python3"];

/// The reference `jupyter` command, if available.
pub fn jupyter() -> Option<PathBuf> {
    let found = std::env::var_os("GANYMEDE_E2E_JUPYTER")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("PATH").and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join("jupyter"))
                    .find(|j| j.is_file())
            })
        });
    if found.is_none() {
        assert!(
            std::env::var("GANYMEDE_E2E_REQUIRE_JUPYTER").as_deref() != Ok("1"),
            "GANYMEDE_E2E_REQUIRE_JUPYTER=1 but no jupyter found"
        );
        eprintln!("skipping: no reference jupyter (set GANYMEDE_E2E_JUPYTER)");
    }
    found
}

/// Replace what legitimately differs between two runs of the same kernel:
/// timestamps and random ids (UUIDs and 32-hex ids), numbered in order of
/// first appearance so structure still has to match.
pub fn normalize(text: &str) -> String {
    let mut ids: HashMap<String, usize> = HashMap::new();
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if let Some(n) = timestamp_len(&b[i..]) {
            out.push_str("<timestamp>");
            i += n;
            continue;
        }
        if let Some(n) = id_len(&b[i..]) {
            if i == 0 || !b[i - 1].is_ascii_alphanumeric() {
                let next = ids.len();
                let k = *ids.entry(text[i..i + n].to_string()).or_insert(next);
                out.push_str(&format!("<id{k}>"));
                i += n;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SS(.f+)?(Z|+00:00)?`
fn timestamp_len(b: &[u8]) -> Option<usize> {
    let pat = b"dddd-dd-ddTdd:dd:dd";
    if b.len() < pat.len() {
        return None;
    }
    for (p, c) in pat.iter().zip(b) {
        let ok = if *p == b'd' {
            c.is_ascii_digit()
        } else {
            p == c
        };
        if !ok {
            return None;
        }
    }
    let mut n = pat.len();
    if b.get(n) == Some(&b'.') {
        n += 1;
        while b.get(n).is_some_and(u8::is_ascii_digit) {
            n += 1;
        }
    }
    if b.get(n) == Some(&b'Z') {
        n += 1;
    } else if b[n..].starts_with(b"+00:00") {
        n += 6;
    }
    Some(n)
}

/// A UUID (8-4-4-4-12 hex) or 32 hex digits, not followed by more
/// alphanumerics.
fn id_len(b: &[u8]) -> Option<usize> {
    let hex = |c: &u8| c.is_ascii_hexdigit();
    let groups = [8, 4, 4, 4, 12];
    let mut n = 0;
    let mut uuid = true;
    for (gi, g) in groups.iter().enumerate() {
        if b.len() < n + g || !b[n..n + g].iter().all(hex) {
            uuid = false;
            break;
        }
        n += g;
        if gi < 4 {
            if b.get(n) != Some(&b'-') {
                uuid = false;
                break;
            }
            n += 1;
        }
    }
    let n = if uuid {
        n
    } else if b.len() >= 32 && b[..32].iter().all(hex) {
        32
    } else {
        return None;
    };
    if b.get(n).is_some_and(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(n)
}

/// A diff-friendly report: the first lines that differ.
pub fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  reference: {x}\n  ganymede:  {y}", i + 1);
        }
    }
    format!(
        "lengths differ: reference {} lines, ganymede {} lines",
        a.lines().count(),
        b.lines().count()
    )
}
