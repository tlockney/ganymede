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

#[cfg(unix)]
#[test]
fn second_ctrl_c_stops_a_cell_that_ignores_interrupts() {
    let child = ganymede(&["run", "-k", "fake", "hang 30000"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let start = std::time::Instant::now();
    for _ in 0..2 {
        unsafe {
            kill(child.id() as i32, 2);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130), "{}", stderr(&out));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    assert!(stderr(&out).contains("Ctrl-C again to stop"));
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

    // Two notebooks of one name would save to one file: refused up front.
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    for d in [&a, &b] {
        std::fs::create_dir(d).unwrap();
        std::fs::copy(&nb, d.join("nb.ipynb")).unwrap();
    }
    std::fs::remove_file(dir.path().join("nb-out.ipynb")).unwrap();
    let out = ganymede(&["execute", "-o", out_tpl.to_str().unwrap()])
        .arg(a.join("nb.ipynb"))
        .arg(b.join("nb.ipynb"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("would both be saved to"),
        "{}",
        stderr(&out)
    );
    assert!(!dir.path().join("nb-out.ipynb").exists());

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
fn execute_params_are_recorded_and_results_printed() {
    // The fake kernel can't read its environment; the real kernels check
    // that the parameters arrive (tests/kernels.rs).
    let dir = tempfile::tempdir().unwrap();
    let nb = dir.path().join("p.ipynb");
    std::fs::write(
        &nb,
        r#"{"nbformat":4,"nbformat_minor":5,"metadata":{"ganymede":{"agent":"x"}},"cells":[
 {"cell_type":"code","id":"a","metadata":{},"source":"one","outputs":[],"execution_count":null}]}"#,
    )
    .unwrap();
    let file = dir.path().join("params.json");
    std::fs::write(&file, r#"{"days": 30, "counter": "Fremont"}"#).unwrap();
    let out_nb = dir.path().join("out.ipynb");
    let out = run_ok(
        ganymede(&[
            "execute",
            "-k",
            "fake",
            "--results",
            "-p",
            "counter=2nd Ave",
        ])
        .args(["-p", "tags=[1,\"a\"]", "-p", "empty=", "--params-file"])
        .arg(&file)
        .arg("-o")
        .arg(&out_nb)
        .arg(&nb),
    );
    let line: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(
        line,
        serde_json::json!({ "path": nb.to_string_lossy(), "status": "ok", "results": {} })
    );
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_nb).unwrap()).unwrap();
    assert_eq!(
        saved["metadata"]["ganymede"],
        serde_json::json!({
            "agent": "x",
            "params": { "days": 30, "counter": "2nd Ave", "tags": [1, "a"], "empty": "" },
        })
    );

    // A later run without parameters drops the earlier run's.
    run_ok(ganymede(&["execute", "-k", "fake", "--inplace"]).arg(&out_nb));
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_nb).unwrap()).unwrap();
    assert_eq!(
        saved["metadata"]["ganymede"],
        serde_json::json!({ "agent": "x" })
    );

    // A run whose notebook can't be written still gets a line.
    let out = ganymede(&["execute", "-k", "fake", "--results", "-o"])
        .arg(dir.path().join("no/such/dir/out.ipynb"))
        .arg(&nb)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let line: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(line["status"], "error");

    // A notebook that can't be read still gets a line.
    let missing = dir.path().join("missing.ipynb");
    let out = ganymede(&["execute", "-k", "fake", "--results"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let line: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(line["status"], "error");
    assert_eq!(line["results"], serde_json::json!({}));
}

#[test]
fn execute_results_come_only_from_cells_the_run_executed() {
    // Saved results from an earlier run: in a cell that runs again (its
    // outputs are replaced), one skipped by tag, and one after a failure.
    let dir = tempfile::tempdir().unwrap();
    let nb = dir.path().join("stale.ipynb");
    let saved = |name: &str| {
        serde_json::json!([{ "output_type": "display_data", "metadata": {},
            "data": { "application/vnd.ganymede.result+json": { "name": name, "value": 1 } } }])
    };
    let cell = |id: &str, source: &str, tags: serde_json::Value, name: &str| {
        serde_json::json!({ "cell_type": "code", "id": id, "metadata": { "tags": tags },
            "source": source, "execution_count": 1, "outputs": saved(name) })
    };
    let doc = |failing: bool| {
        serde_json::json!({
            "nbformat": 4, "nbformat_minor": 5, "metadata": {},
            "cells": [
                cell("a", "one", serde_json::json!([]), "reran"),
                cell("b", "two", serde_json::json!(["skip-execution"]), "skipped"),
                cell("c", if failing { "raise" } else { "two" }, serde_json::json!([]), "failing"),
                cell("d", "two", serde_json::json!([]), "after"),
            ],
        })
    };
    for (failing, code) in [(false, 0), (true, 1)] {
        std::fs::write(&nb, doc(failing).to_string()).unwrap();
        let out = ganymede(&["execute", "-k", "fake", "--results"])
            .arg(&nb)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code), "{}", stderr(&out));
        let line: serde_json::Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        assert_eq!(line["results"], serde_json::json!({}), "failing: {failing}");
    }
}

#[test]
fn execute_param_mistakes_are_reported_before_running() {
    let dir = tempfile::tempdir().unwrap();
    let nb = fake_notebook(dir.path());
    let not_object = dir.path().join("list.json");
    std::fs::write(&not_object, "[1]").unwrap();
    let big = format!("big={}", "x".repeat(40 * 1024));
    let cases: Vec<(Vec<&std::ffi::OsStr>, &str, i32)> = vec![
        (
            vec!["-p".as_ref(), "novalue".as_ref()],
            "expected NAME=VALUE",
            2,
        ),
        (vec!["-p".as_ref(), "=x".as_ref()], "expected NAME=VALUE", 2),
        (
            vec!["--results".as_ref(), "-o".as_ref(), "-".as_ref()],
            "both write to stdout",
            2,
        ),
        (
            vec!["--params-file".as_ref(), not_object.as_os_str()],
            "must hold a JSON object",
            1,
        ),
        (vec!["-p".as_ref(), big.as_ref()], "limit", 1),
    ];
    for (args, says, code) in cases {
        let out = ganymede(&["execute"])
            .args(&args)
            .arg(&nb)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code), "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(says), "{args:?}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("executing"),
            "{args:?} ran: {}",
            stderr(&out)
        );
    }
}

#[test]
fn run_with_empty_stdin_is_a_usage_error() {
    for input in ["", "  \n"] {
        let mut child = ganymede(&["run", "-k", "fake"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{input:?}");
        assert!(stderr(&out).contains("no code"), "{}", stderr(&out));
    }
}

#[test]
fn run_json_marks_a_cell_that_could_not_finish() {
    let out = ganymede(&[
        "run",
        "--json",
        "--timeout",
        "1",
        "-k",
        "fake",
        "x",
        "sleep 5000",
        "y",
    ])
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let lines: Vec<serde_json::Value> = stdout(&out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{}", stdout(&out));
    assert_eq!(lines[0]["status"], "ok");
    assert_eq!(lines[1]["cell"], 1);
    assert_eq!(lines[1]["status"], "incomplete");
    assert!(
        lines[1]["error"].as_str().unwrap().starts_with("cell 1"),
        "{}",
        lines[1]
    );
}

#[test]
fn execute_reports_every_notebook_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.ipynb");
    let bare = dir.path().join("bare.ipynb");
    let nb = r#"{"nbformat":4,"nbformat_minor":5,"metadata":{"kernelspec":{"name":"fake","display_name":"F"}},"cells":[]}"#;
    std::fs::write(&good, nb).unwrap();
    std::fs::write(
        &bare,
        r#"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[]}"#,
    )
    .unwrap();
    let out = ganymede(&["execute"])
        .arg(dir.path().join("missing.ipynb"))
        .arg(&bare)
        .arg(&good)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("missing.ipynb"), "{err}");
    assert!(err.contains("--kernel"), "{err}");
    assert!(
        err.contains("executing") && err.contains("good.ipynb"),
        "{err}"
    );
    assert!(err.contains("0 cells executed"), "{err}");
}

#[test]
fn accepts_jupyter_execute_flag_names() {
    let dir = tempfile::tempdir().unwrap();
    let nb = dir.path().join("n.ipynb");
    std::fs::write(
        &nb,
        r#"{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[]}"#,
    )
    .unwrap();
    run_ok(
        ganymede(&[
            "execute",
            "--kernel_name",
            "fake",
            "--startup_timeout",
            "5",
            "--skip-cells-with-tag",
            "skip",
            "--allow-error-names",
            "E",
        ])
        .arg(&nb),
    );
}

#[test]
fn kernelspec_remove_takes_force_flags() {
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("kernels/k")).unwrap();
    std::fs::write(data.path().join("kernels/k/kernel.json"), FAKE_SPEC).unwrap();
    let mut c = ganymede(&["kernelspec", "remove", "-y", "k"]);
    c.env("JUPYTER_DATA_DIR", data.path())
        .env("JUPYTER_PATH", "")
        .env_remove("XDG_DATA_HOME");
    run_ok(c);
    assert!(!data.path().join("kernels/k").exists());
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

#[test]
fn serve_writes_a_server_info_file_while_running() {
    let root = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let mut child = ganymede(&["serve", "--port", "0", "--token", "secret", "--root-dir"])
        .arg(root.path())
        .env("JUPYTER_RUNTIME_DIR", runtime.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().split('?').next().unwrap().to_string();
    let port = base.rsplit(':').next().unwrap().trim_end_matches('/');

    let file = runtime.path().join(format!("jpserver-{}.json", child.id()));
    let info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).expect("jpserver file")).unwrap();
    assert_eq!(info["url"], base);
    assert_eq!(info["port"].to_string(), port);
    assert_eq!(info["token"], "secret");
    assert_eq!(info["pid"], child.id());
    assert_eq!(
        info["root_dir"],
        root.path().canonicalize().unwrap().to_str().unwrap()
    );
    let open = runtime
        .path()
        .join(format!("jpserver-{}-open.html", child.id()));
    assert!(std::fs::read_to_string(&open)
        .unwrap()
        .contains("?token=secret"));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // `jupyter server list` finds it, as it finds a jupyter_server.
    if let Some(jupyter) = jupyter() {
        let out = run_ok(
            std::process::Command::new(jupyter)
                .args(["server", "list", "--json"])
                .env("JUPYTER_RUNTIME_DIR", runtime.path()),
        );
        let listed: Vec<serde_json::Value> = stdout(&out)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(listed, [info], "{}", stdout(&out));
    }

    unsafe {
        kill(child.id() as i32, 15);
    }
    assert!(child.wait().unwrap().success(), "clean exit on SIGTERM");
    assert!(!file.exists(), "removed on exit");
    assert!(!open.exists(), "removed on exit");
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

#[test]
fn completions_for_each_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let out = stdout(&run_ok(ganymede(&["completions", shell])));
        assert!(out.contains("ganymede"), "{shell}: {out}");
        assert!(out.contains("kernelspec"), "{shell}");
    }
    assert!(!ganymede(&["completions", "tcsh"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn attach_and_interrupt_are_documented() {
    let help = stdout(&run_ok(ganymede(&["--help"])));
    assert!(help.contains("attach"), "{help}");
    assert!(help.contains("completions"), "{help}");
    let help = stdout(&run_ok(ganymede(&["run", "--help"])));
    assert!(help.contains("--interrupt-after-ms"), "{help}");
}

#[test]
fn lab_list_clean_uninstall_and_extension_toggles() {
    let data = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let lab = |args: &[&str]| {
        let mut c = ganymede(args);
        c.env("GANYMEDE_DATA_DIR", data.path())
            .env("JUPYTER_CONFIG_DIR", config.path());
        c
    };
    let json = |args: &[&str]| -> serde_json::Value {
        serde_json::from_slice(&run_ok(lab(args)).stdout).unwrap()
    };
    assert_eq!(json(&["lab", "list", "--json"]), serde_json::json!([]));
    assert_eq!(
        json(&["lab", "extension", "list", "--json"]),
        serde_json::json!([])
    );

    let d = data.path();
    std::fs::create_dir_all(d.join("lab/4.6.4/static")).unwrap();
    std::fs::write(d.join("lab/4.6.4/static/index.html"), "").unwrap();
    std::fs::create_dir_all(d.join("lab/.4.7.0-abc")).unwrap();
    std::fs::write(d.join("lab/.download-1"), "").unwrap();
    std::fs::create_dir_all(d.join("labextensions/.packages")).unwrap();
    std::fs::write(
        d.join("labextensions/.packages/w.json"),
        r#"{"name":"w","version":"1.0","extensions":["w"],"required_by":null}"#,
    )
    .unwrap();
    std::fs::create_dir_all(d.join("labextensions/w")).unwrap();
    std::fs::write(d.join("labextensions/.download-2"), "").unwrap();

    let v = json(&["lab", "list", "--json"]);
    assert_eq!(v[0]["version"], "4.6.4");
    let v = json(&["lab", "extension", "list", "--json"]);
    assert_eq!(v[0]["name"], "w");
    assert_eq!(v[0]["extensions"], serde_json::json!(["w"]));

    let out = run_ok(lab(&["lab", "clean"]));
    assert!(
        stderr(&out).contains("removed 3 leftover"),
        "{}",
        stderr(&out)
    );
    assert!(d.join("lab/4.6.4").is_dir() && d.join("labextensions/.packages/w.json").is_file());
    assert!(!d.join("lab/.download-1").exists() && !d.join("lab/.4.7.0-abc").exists());

    run_ok(lab(&["lab", "extension", "disable", "w", "x:plugin"]));
    run_ok(lab(&["lab", "extension", "enable", "x:plugin"]));
    let page: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config.path().join("labconfig/page_config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        page["disabledExtensions"],
        serde_json::json!({"w": true, "x:plugin": false})
    );

    run_ok(lab(&["lab", "uninstall"]));
    assert!(!d.join("lab").exists() && !d.join("labextensions").exists());
    assert_eq!(json(&["lab", "list", "--json"]), serde_json::json!([]));
}

/// `ganymede mcp` on stdio: newline-delimited JSON-RPC in, answers out.
struct McpClient {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
    next: u64,
}

impl McpClient {
    fn start(mut cmd: std::process::Command) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let mut c = McpClient {
            child,
            stdin,
            lines,
            next: 0,
        };
        let v = c.request(
            "initialize",
            serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "cli-test", "version": "1"}}),
        );
        assert_eq!(v["result"]["serverInfo"]["name"], "ganymede", "{v}");
        c.send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, v: serde_json::Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next += 1;
        let id = self.next;
        self.send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        loop {
            let line = self.lines.next().expect("an answer").unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// A tool's text, failing on an error.
    fn tool(&mut self, name: &str, args: serde_json::Value) -> String {
        let v = self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": args}),
        );
        assert!(
            v.get("error").is_none() && v["result"]["isError"] != true,
            "{v}"
        );
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Closes stdin and waits for the command to stop.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        self.child.wait().unwrap()
    }
}

#[test]
fn mcp_on_stdio_with_its_own_server() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut cmd = ganymede(&["mcp", "--no-execute", "--root-dir"]);
    cmd.arg(root.path())
        .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
        .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
        .env("JUPYTER_DATA_DIR", state.path().join("data"));
    let mut mcp = McpClient::start(cmd);
    let out = mcp.tool(
        "notebook_open",
        serde_json::json!({"path": "notes/log.ipynb", "title": "Log"}),
    );
    assert!(out.starts_with("Created notes/log.ipynb"), "{out}");
    mcp.tool("cell_add", serde_json::json!({"source": "First finding."}));
    let v = mcp.request(
        "tools/call",
        serde_json::json!({"name": "cell_run", "arguments": {"source": "1"}}),
    );
    assert!(v.to_string().contains("execution is turned off"), "{v}");
    // The edit is saved when stdin closes, though the save delay hasn't
    // passed.
    assert!(mcp.finish().success());
    let nb: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("notes/log.ipynb")).unwrap())
            .unwrap();
    let sources: Vec<String> = nb["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| match &c["source"] {
            serde_json::Value::Array(a) => a.iter().map(|s| s.as_str().unwrap()).collect(),
            s => s.as_str().unwrap().to_string(),
        })
        .collect();
    assert_eq!(sources, ["# Log", "First finding."]);
}

#[test]
fn mcp_on_stdio_uses_a_running_server() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(
        root.path().join("sub/a.ipynb"),
        r##"{"cells":[{"cell_type":"markdown","id":"m1","metadata":{},"source":"from disk"}],"metadata":{},"nbformat":4,"nbformat_minor":5}"##,
    )
    .unwrap();
    let env = |c: &mut std::process::Command| {
        c.env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
            .env("JUPYTER_DATA_DIR", state.path().join("data"));
    };
    let mut serve = ganymede(&[
        "serve",
        "--mcp",
        "--port",
        "0",
        "--token",
        "secret",
        "--root-dir",
    ]);
    serve
        .arg(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    env(&mut serve);
    let mut server = serve.spawn().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();

    // Run from a directory under the server's root.
    let mut cmd = ganymede(&["mcp"]);
    cmd.current_dir(root.path().join("sub"));
    env(&mut cmd);
    let mut mcp = McpClient::start(cmd);
    let out = mcp.tool("notebook_read", serde_json::json!({"path": "sub/a.ipynb"}));
    assert!(out.contains("from disk"), "{out}");
    let out = mcp.tool("notebooks_list", serde_json::json!({"dir": "sub"}));
    assert!(out.contains("a.ipynb"), "{out}");
    assert!(mcp.finish().success());
    // No second server: `ganymede mcp` wrote no server info file.
    let files = std::fs::read_dir(state.path().join("runtime"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".json")
        })
        .count();
    assert_eq!(files, 1);
    unsafe {
        kill(server.id() as i32, 15);
    }
    let _ = server.wait();
}

/// When the server has ended the session (here: it restarted), the
/// proxy starts a new one and the call goes through.
#[test]
fn mcp_on_stdio_starts_a_new_session_when_the_server_ends_one() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let start = || {
        let mut serve = ganymede(&["serve", "--mcp", "--token", "secret", "--port"]);
        serve
            .arg(port.to_string())
            .arg("--root-dir")
            .arg(root.path())
            .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
            .env("JUPYTER_DATA_DIR", state.path().join("data"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut server = serve.spawn().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(server.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        server
    };
    let stop = |mut server: std::process::Child| {
        unsafe {
            kill(server.id() as i32, 15);
        }
        server.wait().unwrap();
    };
    let mut server = start();
    let mut cmd = ganymede(&["mcp", "--token", "secret", "--server"]);
    cmd.arg(format!("http://127.0.0.1:{port}/"));
    let mut mcp = McpClient::start(cmd);
    let out = mcp.tool("notebooks_list", serde_json::json!({}));
    assert!(!out.contains("error"), "{out}");
    stop(server);
    server = start();
    std::fs::write(
        root.path().join("after.ipynb"),
        r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
    )
    .unwrap();
    let out = mcp.tool("notebooks_list", serde_json::json!({}));
    assert!(out.contains("after.ipynb"), "{out}");
    assert!(mcp.finish().success());
    stop(server);
}

/// A stand-in MCP server for the proxy's reconnects: it hands out session
/// ids, forgets them all on `end_sessions`, and can refuse the next
/// `initialize`s or answer them slowly.
#[derive(Default)]
struct FakeMcp {
    sessions: Vec<String>,
    started: usize,
    /// `initialize`s received, refused ones too.
    inits: usize,
    refuse_init: usize,
    slow_init: bool,
}

impl FakeMcp {
    /// Serves on a loopback port; returns its `/` URL.
    fn serve(state: std::sync::Arc<std::sync::Mutex<FakeMcp>>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let state = state.clone();
                std::thread::spawn(move || {
                    let _ = FakeMcp::connection(conn.unwrap(), &state);
                });
            }
        });
        url
    }

    fn connection(
        conn: std::net::TcpStream,
        state: &std::sync::Mutex<FakeMcp>,
    ) -> std::io::Result<()> {
        use std::io::Read;
        let mut write = conn.try_clone()?;
        let mut read = std::io::BufReader::new(conn);
        loop {
            let mut start = String::new();
            if read.read_line(&mut start)? == 0 {
                return Ok(());
            }
            let (mut len, mut session) = (0usize, None::<String>);
            loop {
                let mut h = String::new();
                read.read_line(&mut h)?;
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => len = v.trim().parse().unwrap(),
                    "mcp-session-id" => session = Some(v.trim().to_string()),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            read.read_exact(&mut body)?;
            let (status, headers, body) = if start.starts_with("POST") {
                let msg: serde_json::Value = serde_json::from_slice(&body).unwrap();
                // Slow outside the lock, so other calls get their 404 while
                // the new session is starting.
                let slow = msg["method"] == "initialize" && state.lock().unwrap().slow_init;
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                state.lock().unwrap().answer(&msg, session)
            } else {
                (200, String::new(), String::new())
            };
            write!(
                write,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                body.len()
            )?;
        }
    }

    fn answer(
        &mut self,
        msg: &serde_json::Value,
        session: Option<String>,
    ) -> (u16, String, String) {
        let reply = |result: serde_json::Value| {
            serde_json::json!({"jsonrpc": "2.0", "id": msg["id"], "result": result}).to_string()
        };
        match msg["method"].as_str().unwrap() {
            "initialize" => {
                assert!(session.is_none(), "initialize sent in a session");
                self.inits += 1;
                if self.refuse_init > 0 {
                    self.refuse_init -= 1;
                    return (503, String::new(), String::new());
                }
                self.started += 1;
                let id = format!("s{}", self.started);
                self.sessions.push(id.clone());
                let result = serde_json::json!({"protocolVersion": "2025-06-18",
                    "capabilities": {}, "serverInfo": {"name": "ganymede", "version": "0"}});
                (200, format!("Mcp-Session-Id: {id}\r\n"), reply(result))
            }
            _ => match session {
                None => (400, String::new(), "no session".into()),
                Some(s) if !self.sessions.contains(&s) => (404, String::new(), String::new()),
                Some(_) if msg.get("id").is_none() => (202, String::new(), String::new()),
                Some(s) => {
                    let text = format!("in {s}");
                    let result = serde_json::json!({"content": [{"type": "text", "text": text}]});
                    (200, String::new(), reply(result))
                }
            },
        }
    }
}

/// A failed start of a new session doesn't leave the proxy without one:
/// the next call starts it again.
#[test]
fn mcp_on_stdio_recovers_when_a_new_session_fails_to_start() {
    let fake = std::sync::Arc::new(std::sync::Mutex::new(FakeMcp::default()));
    let mut cmd = ganymede(&["mcp", "--server"]);
    cmd.arg(FakeMcp::serve(fake.clone()));
    let mut mcp = McpClient::start(cmd);
    assert_eq!(mcp.tool("notebooks_list", serde_json::json!({})), "in s1");
    {
        let mut f = fake.lock().unwrap();
        f.sessions.clear();
        f.refuse_init = 1;
    }
    let v = mcp.request(
        "tools/call",
        serde_json::json!({"name": "notebooks_list", "arguments": {}}),
    );
    assert!(v.to_string().contains("503"), "{v}");
    assert_eq!(mcp.tool("notebooks_list", serde_json::json!({})), "in s2");
    assert!(mcp.finish().success());
}

/// Calls sent while the proxy starts a new session wait for it instead of
/// going without one, and only one new session is started.
#[test]
fn mcp_on_stdio_calls_during_a_reconnect_wait_for_it() {
    let fake = std::sync::Arc::new(std::sync::Mutex::new(FakeMcp::default()));
    let mut cmd = ganymede(&["mcp", "--server"]);
    cmd.arg(FakeMcp::serve(fake.clone()));
    let mut mcp = McpClient::start(cmd);
    {
        let mut f = fake.lock().unwrap();
        f.sessions.clear();
        f.slow_init = true;
    }
    for id in 100..103 {
        mcp.send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "notebooks_list", "arguments": {}}}),
        );
        // The first call is in its reconnect by the time the others go.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut answers = Vec::new();
    while answers.len() < 3 {
        let line = mcp.lines.next().expect("an answer").unwrap();
        answers.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
    }
    for v in &answers {
        assert_eq!(v["result"]["content"][0]["text"], "in s2", "{v}");
    }
    assert_eq!(fake.lock().unwrap().started, 2);
    assert!(mcp.finish().success());
}

/// When starting a new session fails, the calls that were waiting for it
/// fail too, rather than each trying again in turn; a later call tries
/// again.
#[test]
fn mcp_on_stdio_calls_waiting_on_a_failed_reconnect_fail_together() {
    let fake = std::sync::Arc::new(std::sync::Mutex::new(FakeMcp::default()));
    let mut cmd = ganymede(&["mcp", "--server"]);
    cmd.arg(FakeMcp::serve(fake.clone()));
    let mut mcp = McpClient::start(cmd);
    {
        let mut f = fake.lock().unwrap();
        f.sessions.clear();
        f.slow_init = true;
        f.refuse_init = 10;
    }
    for id in 100..103 {
        mcp.send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "notebooks_list", "arguments": {}}}),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for _ in 0..3 {
        let line = mcp.lines.next().expect("an answer").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("error").is_some(), "{v}");
    }
    // The client's initialize, then one try for all three calls.
    assert_eq!(fake.lock().unwrap().inits, 2);
    {
        let mut f = fake.lock().unwrap();
        f.slow_init = false;
        f.refuse_init = 0;
    }
    assert_eq!(mcp.tool("notebooks_list", serde_json::json!({})), "in s2");
    assert!(mcp.finish().success());
}

/// Requests still running when stdin closes are answered, and
/// SIGTERM saves open notebooks as stdin closing does.
#[test]
fn mcp_on_stdio_finishes_requests_and_saves_on_sigterm() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let cmd = || {
        let mut cmd = ganymede(&["mcp", "--no-execute", "--root-dir"]);
        cmd.arg(root.path())
            .env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
            .env("JUPYTER_DATA_DIR", state.path().join("data"));
        cmd
    };
    let sources = |name: &str| -> Vec<String> {
        let nb: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join(name)).unwrap()).unwrap();
        nb["cells"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| match &c["source"] {
                serde_json::Value::Array(a) => a.iter().map(|s| s.as_str().unwrap()).collect(),
                s => s.as_str().unwrap().to_string(),
            })
            .collect()
    };

    // Two notes sent without waiting, then EOF without reading.
    let mut mcp = McpClient::start(cmd());
    mcp.tool("notebook_open", serde_json::json!({"path": "a.ipynb"}));
    for (i, src) in ["one", "two"].into_iter().enumerate() {
        mcp.send(
            serde_json::json!({"jsonrpc": "2.0", "id": 10 + i, "method": "tools/call",
            "params": {"name": "cell_add", "arguments": {"source": src}}}),
        );
    }
    drop(mcp.stdin.take());
    let mut answered = Vec::new();
    for line in mcp.lines.by_ref() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        assert!(v.get("error").is_none(), "{v}");
        answered.push(v["id"].as_u64().unwrap());
    }
    answered.sort();
    assert_eq!(answered, [10, 11]);
    assert!(mcp.child.wait().unwrap().success());
    let mut got = sources("a.ipynb");
    got.sort();
    assert_eq!(got, ["one", "two"]);

    // SIGTERM within the save delay.
    let mut mcp = McpClient::start(cmd());
    mcp.tool("notebook_open", serde_json::json!({"path": "b.ipynb"}));
    mcp.tool("cell_add", serde_json::json!({"source": "kept"}));
    unsafe {
        kill(mcp.child.id() as i32, 15);
    }
    mcp.child.wait().unwrap();
    assert_eq!(sources("b.ipynb"), ["kept"]);
}

/// `ganymede mcp call` calls a tool on an agent's own server, found by
/// its announcement file, for hooks; with no server it fails (#315).
#[test]
fn mcp_call_reaches_an_agents_own_server() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let env = |c: &mut std::process::Command| {
        c.env("JUPYTER_RUNTIME_DIR", state.path().join("runtime"))
            .env("JUPYTER_CONFIG_DIR", state.path().join("config"))
            .env("JUPYTER_DATA_DIR", state.path().join("data"))
            .env_remove("JUPYTER_TOKEN");
    };
    let call = |args: &[&str]| {
        let mut c = ganymede(&["mcp", "call"]);
        c.args(args).current_dir(root.path());
        env(&mut c);
        c.output().unwrap()
    };
    let out = call(&["notebooks_list"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no server"),
        "{out:?}"
    );

    let mut cmd = ganymede(&["mcp", "--no-execute", "--root-dir"]);
    cmd.arg(root.path());
    env(&mut cmd);
    let mut mcp = McpClient::start(cmd);
    let opened = mcp.tool(
        "notebook_open",
        serde_json::json!({"path": "j.ipynb", "title": "J"}),
    );
    let mark = opened
        .lines()
        .find_map(|l| l.strip_prefix("@@read "))
        .unwrap()
        .to_string();
    // A hook records a step for the agent, then asks what changed.
    let args = serde_json::json!({"path": "j.ipynb", "source": "$ ls", "mode": "recorded",
                                  "mark": mark});
    let out = call(&["cell_add", &args.to_string(), "--client", "hook"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Added cell"));
    let args = serde_json::json!({"path": "j.ipynb", "since": mark});
    let out = call(&["notebook_read", &args.to_string()]);
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("No changes by others"),
        "{out:?}"
    );
    // A tool's error exits 1 with its message.
    let out = call(&["cell_delete", r#"{"path": "j.ipynb", "id": "nope"}"#]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("no cell nope"));
    assert!(mcp.finish().success());
    // The announcement goes with the server.
    let left: Vec<_> = std::fs::read_dir(state.path().join("runtime"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("ganymede-mcp-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
