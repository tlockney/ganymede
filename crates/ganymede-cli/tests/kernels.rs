//! The same scenarios on every real kernel: cdp-kernel (TypeScript on
//! Node) and ipykernel. Only the code differs per language; what each
//! scenario checks does not. Runs through the engine library, and through
//! the CLI where the CLI adds something (standalone kernels, attaching).

mod common;

use common::*;
use ganymede_engine::protocol::Channel;
use ganymede_engine::{Engine, ExecuteOptions, Handler, IsComplete, Kernel, StartOptions};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Code for each scenario, per language.
struct Lang {
    kernel: &'static str,
    language: &'static str,
    /// Evaluates to 42.
    answer: &'static str,
    stdout: &'static str,
    stderr: &'static str,
    /// Raises an error named `ename`.
    raise: &'static str,
    ename: &'static str,
    /// Asks for input with prompt `name? ` and prints `hi <name>`.
    input: &'static str,
    /// Sleeps 30 s.
    sleep: &'static str,
    set_state: &'static str,
    read_state: &'static str,
    /// Error name when `read_state` runs on a fresh kernel.
    undefined: &'static str,
    /// Registers comm target `echo`, which sends each message back.
    echo_target: &'static str,
    /// Prints the working directory.
    cwd: &'static str,
    /// Defines something to complete, and a prefix of its name.
    complete_setup: &'static str,
    complete_prefix: &'static str,
    complete_match: &'static str,
    incomplete: &'static str,
    inspect: &'static str,
}

const LANGS: &[Lang] = &[
    Lang {
        kernel: "cdp-node",
        language: "typescript",
        answer: "6 * 7",
        stdout: "console.log('out')",
        stderr: "console.error('err')",
        raise: "throw new TypeError('bad')",
        ename: "TypeError",
        input: "console.log('hi ' + prompt('name? '))",
        sleep: "await new Promise((r) => setTimeout(r, 30000))",
        set_state: "var kept = 'state'",
        read_state: "kept",
        undefined: "ReferenceError",
        echo_target: "jupyter.comm.registerTarget('echo', (comm) => comm.onMsg((m) => comm.send(m.data)))",
        cwd: "console.log(process.cwd())",
        complete_setup: "const zebrafish = 1",
        complete_prefix: "zebraf",
        complete_match: "zebrafish",
        incomplete: "function f() {",
        inspect: "Math",
    },
    Lang {
        kernel: "python3",
        language: "python",
        answer: "6 * 7",
        stdout: "print('out')",
        stderr: "import sys; print('err', file=sys.stderr)",
        raise: "raise TypeError('bad')",
        ename: "TypeError",
        input: "print('hi ' + input('name? '))",
        sleep: "import time; time.sleep(30)",
        set_state: "kept = 'state'",
        read_state: "kept",
        undefined: "NameError",
        echo_target: "def _echo(comm, msg):\n    comm.on_msg(lambda m: comm.send(m['content']['data']))\nget_ipython().kernel.comm_manager.register_target('echo', _echo)",
        cwd: "import os; print(os.getcwd())",
        complete_setup: "zebrafish = 1",
        complete_prefix: "zebraf",
        complete_match: "zebrafish",
        incomplete: "for i in range(3):",
        inspect: "len",
    },
];

fn langs() -> impl Iterator<Item = &'static Lang> {
    LANGS.iter().filter(|l| have_kernel(l.kernel))
}

fn engine() -> Engine {
    Engine::builder().quiet(true).build()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn plain(r: &ganymede_engine::ExecuteResult) -> String {
    strip_ansi(&r.text())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        match (esc, c) {
            (false, '\u{1b}') => esc = true,
            (true, c) if c.is_ascii_alphabetic() => esc = false,
            (true, _) => {}
            (false, c) => out.push(c),
        }
    }
    out
}

#[test]
fn launch_and_kernel_info_pass_through() {
    for l in langs() {
        rt().block_on(async {
            let e = engine();
            let spec = e.kernelspec(l.kernel).unwrap();
            assert_eq!(spec.language, l.language);
            // Kernelspec metadata reaches the host untouched.
            let raw: Value = serde_json::from_str(
                &std::fs::read_to_string(spec.resource_dir.join("kernel.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&spec.metadata).unwrap(),
                raw.get("metadata").cloned().unwrap_or(json!({})),
                "{}",
                l.kernel
            );

            let k = e.start(l.kernel).await.unwrap();
            let info = k.info();
            assert!(info.protocol_version.starts_with('5'), "{}", l.kernel);
            assert_eq!(info.language_info.name, l.language);
            // Every field the kernel sent survives the typed model.
            let typed = serde_json::to_value(&info).unwrap();
            for (key, value) in k.info_json().as_object().unwrap() {
                assert_eq!(typed.get(key), Some(value), "{}: {key}", l.kernel);
            }
            k.shutdown().await.unwrap();
            assert!(!k.is_alive());
        });
    }
}

#[test]
fn execute_results_streams_and_errors() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let r = k.execute(l.answer).await.unwrap();
            assert!(r.is_ok());
            assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            assert_eq!(r.execution_count, Some(1));

            let r = k.execute(l.stdout).await.unwrap();
            assert_eq!(plain(&r), "out\n", "{}", l.kernel);
            let r = k.execute(l.stderr).await.unwrap();
            let names: Vec<_> = r
                .messages
                .iter()
                .filter(|m| m.msg_type() == "stream")
                .map(|m| m.content["name"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(names, ["stderr"], "{}", l.kernel);

            let r = k.execute(l.raise).await.unwrap();
            assert_eq!(r.error(), Some((l.ename, "bad")), "{}", l.kernel);
            assert!(
                matches!(r.outputs.last(), Some(ganymede_engine::Output::Error(e)) if e.ename == l.ename)
            );
            k.shutdown().await.unwrap();
        });
    }
}

struct Answer(Vec<String>);
impl Handler for Answer {
    fn on_input(&mut self, prompt: &str, password: bool) -> Option<String> {
        self.0.push(prompt.to_string());
        assert!(!password);
        Some("Ada".into())
    }
}

#[test]
fn stdin_round_trip() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let mut h = Answer(vec![]);
            let opts = ExecuteOptions {
                allow_stdin: true,
                ..Default::default()
            };
            let r = k.execute_with(l.input, &opts, &mut h).await.unwrap();
            assert_eq!(h.0, ["name? "], "{}", l.kernel);
            assert_eq!(plain(&r), "hi Ada\n", "{}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn interrupt_stops_the_cell_and_keeps_state() {
    for l in langs() {
        rt().block_on(async {
            let k = Arc::new(engine().start(l.kernel).await.unwrap());
            assert!(k.execute(l.set_state).await.unwrap().is_ok());
            let k2 = k.clone();
            let started = std::time::Instant::now();
            let run = tokio::spawn(async move { k2.execute(l.sleep).await });
            tokio::time::sleep(Duration::from_millis(1500)).await;
            k.interrupt().await.unwrap();
            let r = tokio::time::timeout(Duration::from_secs(20), run)
                .await
                .expect("interrupt did not end the cell")
                .unwrap()
                .unwrap();
            assert!(!r.is_ok(), "{}", l.kernel);
            assert!(started.elapsed() < Duration::from_secs(25));
            let r = k.execute(l.read_state).await.unwrap();
            assert!(plain(&r).contains("state"), "{}: {}", l.kernel, plain(&r));
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn restart_loses_state_and_keeps_the_working_directory() {
    for l in langs() {
        rt().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let cwd = dir.path().canonicalize().unwrap();
            let k = engine()
                .start_with(
                    l.kernel,
                    StartOptions {
                        cwd: Some(cwd.clone()),
                        env: vec![],
                    },
                )
                .await
                .unwrap();
            let here = |r: ganymede_engine::ExecuteResult| plain(&r).trim().to_string();
            assert_eq!(here(k.execute(l.cwd).await.unwrap()), cwd.to_string_lossy());
            assert!(k.execute(l.set_state).await.unwrap().is_ok());
            k.restart().await.unwrap();
            let r = k.execute(l.read_state).await.unwrap();
            assert_eq!(r.error().map(|e| e.0), Some(l.undefined), "{}", l.kernel);
            assert_eq!(r.execution_count, Some(1), "{}: counter resets", l.kernel);
            assert_eq!(here(k.execute(l.cwd).await.unwrap()), cwd.to_string_lossy());
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn comms_round_trip() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            assert!(k.execute(l.echo_target).await.unwrap().is_ok());
            let comms = k.comm_info(None).await.unwrap();
            assert!(comms.is_object(), "{}: {comms}", l.kernel);

            let mut sub = k.session().subscribe();
            let id = "0123456789abcdef0123456789abcdef";
            k.comm_open(id, "echo", json!({})).await.unwrap();
            k.comm_msg(id, json!({"ping": 1})).await.unwrap();
            let echoed = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let (ch, m) = sub.recv().await.unwrap();
                    if ch == Channel::IoPub
                        && m.msg_type() == "comm_msg"
                        && m.content["comm_id"] == id
                    {
                        return m.content["data"].clone();
                    }
                }
            })
            .await
            .expect("no echo");
            assert_eq!(echoed, json!({"ping": 1}), "{}", l.kernel);
            k.comm_close(id, json!({})).await.unwrap();
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn complete_inspect_and_is_complete() {
    for l in langs() {
        rt().block_on(async {
            let k: Kernel = engine().start(l.kernel).await.unwrap();
            k.execute(l.complete_setup).await.unwrap();
            let r = k
                .complete(l.complete_prefix, l.complete_prefix.len())
                .await
                .unwrap();
            let matches: Vec<&str> = r["matches"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert!(
                matches.contains(&l.complete_match),
                "{}: {matches:?}",
                l.kernel
            );
            assert_eq!(r["cursor_end"], l.complete_prefix.len());

            let r = k.inspect(l.inspect, l.inspect.len(), 0).await.unwrap();
            assert_eq!(r["found"], true, "{}: {r}", l.kernel);
            assert!(r["data"]["text/plain"].is_string());

            assert_eq!(k.is_complete(l.answer).await.unwrap(), IsComplete::Complete);
            assert!(
                matches!(
                    k.is_complete(l.incomplete).await.unwrap(),
                    IsComplete::Incomplete { .. }
                ),
                "{}",
                l.kernel
            );
            k.shutdown().await.unwrap();
        });
    }
}

// ----- through the CLI ---------------------------------------------------

#[test]
fn cli_run_reports_and_exits_by_status() {
    for l in langs() {
        let out = run_ok(ganymede(&["run", "-q", "-k", l.kernel, l.answer, l.stdout]));
        assert_eq!(strip_ansi(&stdout(&out)), "42\nout\n", "{}", l.kernel);

        let out = ganymede(&["run", "-q", "-k", l.kernel, l.raise, l.stdout])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", l.kernel);
        assert!(stdout(&out).is_empty(), "stops at the error");
        assert!(stderr(&out).contains("bad"), "{}", stderr(&out));

        let out = run_ok(ganymede(&["run", "-q", "--json", "-k", l.kernel, l.answer]));
        let v: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["outputs"][0]["output_type"], "execute_result");
    }
}

#[test]
fn cli_console_runs_piped_multiline_input() {
    use std::io::Write;
    for l in langs() {
        let script = match l.language {
            "python" => "for i in range(2):\n    print('line', i)\n\nprint('done')\n",
            _ => "for (let i = 0; i < 2; i++) {\n  console.log('line ' + i);\n}\nconsole.log('done');\n",
        };
        let mut child = ganymede(&["console", "-q", "-k", l.kernel])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}: {}", l.kernel, stderr(&out));
        let text = strip_ansi(&stdout(&out))
            .replace("line 0", "line_0")
            .replace("line 1", "line_1");
        assert!(
            text.contains("line_0") && text.contains("line_1") && text.ends_with("done\n"),
            "{}: {text:?}",
            l.kernel
        );
    }
}

#[cfg(unix)]
#[test]
fn cli_standalone_kernel_and_attach() {
    use std::io::BufRead;
    for l in langs() {
        let mut child = ganymede(&["kernel", "-q", "-k", l.kernel])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let cf = line.trim().to_string();
        assert!(std::path::Path::new(&cf).exists(), "{cf}");

        run_ok(ganymede(&["run", "--existing", &cf, l.set_state]));
        // A second client sees the first one's state: same kernel.
        let out = run_ok(ganymede(&["run", "--existing", &cf, l.read_state]));
        assert!(strip_ansi(&stdout(&out)).contains("state"), "{}", l.kernel);

        unsafe {
            libc_kill(child.id() as i32, 15);
        }
        let status = child.wait().unwrap();
        assert!(status.success(), "{}: {status}", l.kernel);
        assert!(
            !std::path::Path::new(&cf).exists(),
            "connection file removed on shutdown"
        );
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
