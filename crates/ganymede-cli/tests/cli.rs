//! Every subcommand, end to end through the binary, with nothing installed:
//! the built-in `fake` kernel, temporary data dirs, and the server on a
//! free port. Real kernels are covered in `kernels.rs` and `parity.rs`.

mod common;

use common::*;
use std::io::{BufRead, Write};
use std::process::Stdio;

const FAKE_SPEC: &str = r#"{"argv":["true","{connection_file}"],"display_name":"Spec Test","language":"none","metadata":{"capabilities":{"jit":false}}}"#;

#[test]
fn kernelspec_install_list_remove() {
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let spec = src.path().join("Spec-Test");
    std::fs::create_dir(&spec).unwrap();
    std::fs::write(spec.join("kernel.json"), FAKE_SPEC).unwrap();
    let env = |c: &mut std::process::Command| {
        c.env("JUPYTER_DATA_DIR", data.path())
            .env("JUPYTER_PATH", "")
            .env_remove("XDG_DATA_HOME");
    };

    let mut c = ganymede(&["kernelspec", "install"]);
    c.arg(&spec);
    env(&mut c);
    let out = run_ok(c);
    assert!(stdout(&out).trim().ends_with("kernels/spec-test"));

    let mut c = ganymede(&["kernelspec", "list", "--json"]);
    env(&mut c);
    let v: serde_json::Value = serde_json::from_slice(&run_ok(c).stdout).unwrap();
    let s = &v["kernelspecs"]["spec-test"];
    assert_eq!(s["spec"]["display_name"], "Spec Test");
    assert_eq!(s["spec"]["metadata"]["capabilities"]["jit"], false);
    assert!(s["resource_dir"].as_str().unwrap().ends_with("spec-test"));

    let mut c = ganymede(&["kernelspec", "list"]);
    env(&mut c);
    assert!(stdout(&run_ok(c)).contains("spec-test  Spec Test"));

    let mut c = ganymede(&["kernelspec", "install"]);
    c.arg(&spec);
    env(&mut c);
    assert!(
        !c.output().unwrap().status.success(),
        "exists without --replace"
    );

    let mut c = ganymede(&["kernelspec", "remove", "SPEC-TEST"]);
    env(&mut c);
    run_ok(c);
    let mut c = ganymede(&["kernelspec", "list", "--json"]);
    env(&mut c);
    let v: serde_json::Value = serde_json::from_slice(&run_ok(c).stdout).unwrap();
    assert!(v["kernelspecs"].get("spec-test").is_none());

    let mut c = ganymede(&["kernelspec", "paths"]);
    env(&mut c);
    let paths = stdout(&run_ok(c));
    assert!(paths
        .lines()
        .next()
        .unwrap()
        .starts_with(data.path().to_str().unwrap()));
}

#[test]
fn run_from_args_files_and_stdin() {
    let out = run_ok(ganymede(&["run", "-k", "fake", "one", "two"]));
    assert_eq!(stdout(&out), "echo: one\nONE\necho: two\nTWO\n");

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("cell.txt");
    std::fs::write(&f, "from file").unwrap();
    let out = run_ok(ganymede(&["run", "-k", "fake", "-f"]).arg(&f));
    assert_eq!(stdout(&out), "echo: from file\nFROM FILE\n");

    let mut child = ganymede(&["run", "-k", "fake"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"piped").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(stdout(&out), "echo: piped\nPIPED\n");
}

#[test]
fn run_exit_status_keep_going_and_json() {
    let out = ganymede(&["run", "-k", "fake", "raise", "after"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("FakeError"));

    let out = ganymede(&["run", "-k", "fake", "--keep-going", "raise", "after"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "echo: after\nAFTER\n");

    let out = ganymede(&["run", "-k", "fake", "--json", "a", "raise"])
        .output()
        .unwrap();
    let lines: Vec<serde_json::Value> = stdout(&out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["status"], "ok");
    assert_eq!(lines[0]["execution_count"], 1);
    assert_eq!(lines[0]["outputs"][1]["output_type"], "execute_result");
    assert_eq!(lines[1]["status"], "error");
    assert_eq!(lines[1]["ename"], "FakeError");
}

#[test]
fn run_interrupt() {
    let out = ganymede(&[
        "run",
        "-k",
        "fake",
        "--interrupt-after-ms",
        "200",
        "sleep 5000",
    ])
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("KeyboardInterrupt"),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn run_stops_on_sigterm_and_sighup() {
    for (sig, code) in [(15, 143), (1, 129)] {
        let mut child = ganymede(&["run", "-k", "fake", "sleep 30000"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        unsafe {
            kill(child.id() as i32, sig);
        }
        let start = std::time::Instant::now();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(code), "signal {sig}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }
}

#[test]
fn existing_searches_the_runtime_dir_and_errors_print_once() {
    let rt = tempfile::tempdir().unwrap();
    let out = ganymede(&["run", "--existing", "kernel-nosuch.json", "x"])
        .env("JUPYTER_RUNTIME_DIR", rt.path())
        .current_dir(rt.path().parent().unwrap())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("here or in"), "{err}");
    assert!(err.contains(&rt.path().display().to_string()), "{err}");

    let out = ganymede(&["execute", "-k", "fake", "nosuch.ipynb"])
        .current_dir(rt.path())
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(err.matches("(os error 2)").count(), 1, "{err}");
}

#[test]
fn verbose_reports_kernel_and_status() {
    let out = run_ok(ganymede(&["run", "-v", "-k", "fake", "x"]));
    let err = stderr(&out);
    assert!(err.contains("kernel: fake"), "{err}");
    assert!(err.contains("[1] ok"), "{err}");
}

#[test]
fn console_piped() {
    let mut child = ganymede(&["console", "-k", "fake"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first\n\nsecond \\\ncontinued\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "echo: first\nFIRST\necho: second \\\ncontinued\nSECOND \\\nCONTINUED\n"
    );
}

fn fake_notebook(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("nb.ipynb");
    std::fs::write(
        &p,
        r##"{"nbformat":4,"nbformat_minor":5,"metadata":{"kernelspec":{"name":"fake","display_name":"Fake","language":"fake"}},"cells":[
 {"cell_type":"markdown","id":"m","metadata":{},"source":"# hi"},
 {"cell_type":"code","id":"a","metadata":{"collapsed":false},"source":"one","outputs":[],"execution_count":null},
 {"cell_type":"code","id":"b","metadata":{"tags":["raises-exception"]},"source":"raise","outputs":[],"execution_count":null},
 {"cell_type":"code","id":"c","metadata":{},"source":"two","outputs":[],"execution_count":null}
]}"##,
    )
    .unwrap();
    p
}

#[test]
fn execute_outputs_inplace_stdout_and_failures() {
    let dir = tempfile::tempdir().unwrap();
    let nb = fake_notebook(dir.path());
    let before = std::fs::read_to_string(&nb).unwrap();

    // No destination: checks that it runs, writes nothing.
    run_ok(ganymede(&["execute"]).arg(&nb));
    assert_eq!(std::fs::read_to_string(&nb).unwrap(), before);

    // To stdout.
    let out = run_ok(ganymede(&["execute", "-o", "-"]).arg(&nb));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["metadata"]["language_info"]["name"], "fake");
    assert_eq!(v["cells"][1]["execution_count"], 1);
    assert_eq!(v["cells"][2]["outputs"][0]["output_type"], "error");
    assert_eq!(v["cells"][3]["execution_count"], 3);
    assert!(v["cells"][3]["metadata"]["execution"]["iopub.status.idle"].is_string());

    // {notebook_name} and --no-timing.
    let out_tpl = dir.path().join("{notebook_name}-out.ipynb");
    run_ok(ganymede(&["execute", "--no-timing", "-o", out_tpl.to_str().unwrap()]).arg(&nb));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("nb-out.ipynb")).unwrap())
            .unwrap();
    assert!(v["cells"][1]["metadata"].get("execution").is_none());

    // In place, then cleared again.
    run_ok(ganymede(&["execute", "--inplace"]).arg(&nb));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&nb).unwrap()).unwrap();
    assert_eq!(
        v["cells"][1]["outputs"][1]["data"]["text/plain"],
        serde_json::json!(["ONE"])
    );
    run_ok(ganymede(&["nb", "clear"]).arg(&nb));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&nb).unwrap()).unwrap();
    assert_eq!(v["cells"][1]["outputs"], serde_json::json!([]));
    assert!(v["cells"][1]["execution_count"].is_null());
    assert!(v["cells"][1]["metadata"].get("collapsed").is_none());

    // --force-raise-errors overrides the tag: fails, saves nothing.
    let failed = dir.path().join("failed.ipynb");
    let out = ganymede(&["execute", "--force-raise-errors", "-o"])
        .arg(&failed)
        .arg(&nb)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("FakeError"), "{}", stderr(&out));
    assert!(
        !failed.exists(),
        "a failed run is not saved, as with nbclient"
    );
}

#[test]
fn execute_needs_a_kernel_name() {
    let dir = tempfile::tempdir().unwrap();
    let nb = dir.path().join("bare.ipynb");
    std::fs::write(
        &nb,
        r#"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[]}"#,
    )
    .unwrap();
    let out = ganymede(&["execute"]).arg(&nb).output().unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--kernel"), "{}", stderr(&out));
    run_ok(ganymede(&["execute", "-k", "fake"]).arg(&nb));
}

#[test]
fn info_json() {
    let data = tempfile::tempdir().unwrap();
    let jupyter = tempfile::tempdir().unwrap();
    // A JupyterLab install and an extension, as `lab install` lays them out.
    let lab = data.path().join("lab/4.6.4/static");
    std::fs::create_dir_all(&lab).unwrap();
    std::fs::write(lab.join("index.html"), "").unwrap();
    let ext = data.path().join("labextensions/@s/e");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(
        ext.join("package.json"),
        r#"{"name":"@s/e","version":"1.0.0","jupyterlab":{"_build":{"load":"x.js"}}}"#,
    )
    .unwrap();
    let info = |args: &[&str]| {
        let mut c = ganymede(args);
        c.env("GANYMEDE_DATA_DIR", data.path())
            .env("JUPYTER_DATA_DIR", jupyter.path())
            .env("JUPYTER_PATH", "");
        c
    };
    let out = run_ok(info(&[
        "info",
        "--json",
        "--kernelspec-root",
        "/extra-root",
    ]));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["data_dirs"].is_array());
    let path = |p: &std::path::Path| serde_json::json!(p);
    assert_eq!(v["data_dir"], path(data.path()));
    assert_eq!(v["lab_dir"], path(&data.path().join("lab")));
    assert_eq!(v["lab_versions"], serde_json::json!(["4.6.4"]));
    assert_eq!(
        v["labextensions_dir"],
        path(&data.path().join("labextensions"))
    );
    assert_eq!(v["extensions"][0]["name"], "@s/e");
    assert_eq!(v["extensions"][0]["version"], "1.0.0");
    assert_eq!(v["notary_dir"], path(jupyter.path()));
    assert_eq!(v["kernelspec_roots"][0], "/extra-root");
    assert_eq!(v["kernelspec_roots"][1], path(jupyter.path()));
    assert!(v["build"]["target"].as_str().is_some_and(|t| !t.is_empty()));

    let text = stdout(&run_ok(info(&["info"])));
    assert!(text.contains("(4.6.4)"), "{text}");
    assert!(text.contains("  @s/e 1.0.0"), "{text}");

    assert!(text.contains("build "), "{text}");
    // Scripts parse `--version`; it stays one line.
    let version = stdout(&run_ok(ganymede(&["--version"])));
    assert_eq!(version, format!("ganymede {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn serve_answers_the_api() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("hello.txt"), "hi").unwrap();
    let mut child = ganymede(&["serve", "--port", "0", "--token", "secret", "--root-dir"])
        .arg(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("?token=secret"), "{line}");
    let base = line.trim().split('?').next().unwrap().to_string();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let http = reqwest::Client::new();
        let r = http
            .get(format!("{base}api/contents/hello.txt"))
            .header("Authorization", "token secret")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["content"], "hi");
        let r = http
            .get(format!("{base}api/contents/hello.txt"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
    });
    unsafe {
        kill(child.id() as i32, 15);
    }
    assert!(child.wait().unwrap().success(), "clean exit on SIGTERM");
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn trust_signs_once_and_reset_forgets() {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let nb = dir.path().join("a.ipynb");
    std::fs::write(
        &nb,
        r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [
            {"cell_type": "code", "id": "c", "metadata": {}, "source": "x", "execution_count": 1,
             "outputs": [{"output_type": "display_data", "metadata": {}, "data": {"text/html": "<b>x</b>"}}]}]}"#,
    )
    .unwrap();
    let trust = |args: &[&str]| {
        let mut c = ganymede(&["trust"]);
        c.args(args).env("JUPYTER_DATA_DIR", data.path());
        c
    };
    let nb_arg = nb.to_str().unwrap();

    let out = run_ok(trust(&[nb_arg]));
    assert_eq!(stdout(&out), format!("Signing notebook: {nb_arg}\n"));
    assert!(data.path().join("notebook_secret").is_file());
    assert!(data.path().join("nbsignatures.db").is_file());
    let out = run_ok(trust(&[nb_arg]));
    assert_eq!(stdout(&out), format!("Notebook already signed: {nb_arg}\n"));

    // From stdin.
    let mut c = trust(&[]);
    c.stdin(std::fs::File::open(&nb).unwrap());
    assert_eq!(stdout(&run_ok(c)), "Notebook already signed: <stdin>\n");

    let out = trust(&["missing.ipynb"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("Notebook missing: missing.ipynb"));

    let out = run_ok(trust(&["--reset"]));
    assert!(stdout(&out).contains("Removing trusted signature cache"));
    assert!(stdout(&out).contains("Generating new notebook key"));
    let out = run_ok(trust(&[nb_arg]));
    assert_eq!(stdout(&out), format!("Signing notebook: {nb_arg}\n"));

    // --notary-dir overrides the data dir.
    let other = tempfile::tempdir().unwrap();
    let out = run_ok(trust(&[
        "--notary-dir",
        other.path().to_str().unwrap(),
        nb_arg,
    ]));
    assert_eq!(stdout(&out), format!("Signing notebook: {nb_arg}\n"));
}
