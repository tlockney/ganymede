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
    /// Asks for input with `allow_stdin: false`; prints `no_stdin_says`.
    no_stdin: &'static str,
    no_stdin_says: &'static str,
    /// Ends the language runtime mid-cell. cdp-kernel relaunches its
    /// runtime for the next cell; for ipykernel the runtime is the kernel.
    exit: &'static str,
    exit_kills_kernel: bool,
    /// Registers comm target `closer`, which records the data of the
    /// frontend's `comm_close`; `read_closed` prints it as JSON.
    closer_target: &'static str,
    read_closed: &'static str,
    /// Registers comm target `boom`, whose handler throws `kaboom`.
    boom_target: &'static str,
    /// Prints `before`, displays markdown `one` with display id `d1`,
    /// updates it to `two`, prints `after`.
    display_update: &'static str,
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
        echo_target: "jupyter.comm.registerTarget('echo', (comm) => comm.onMsg((m) => comm.send(m.data, { buffers: m.buffers })))",
        cwd: "console.log(process.cwd())",
        complete_setup: "const zebrafish = 1",
        complete_prefix: "zebraf",
        complete_match: "zebrafish",
        incomplete: "function f() {",
        inspect: "Math",
        no_stdin: "console.log(prompt('x') === null)",
        no_stdin_says: "true",
        exit: "process.exit(0)",
        exit_kills_kernel: false,
        closer_target: "globalThis.closedWith = null; jupyter.comm.registerTarget('closer', (comm) => comm.onClose((m) => { closedWith = m.data; }))",
        read_closed: "console.log(JSON.stringify(closedWith))",
        boom_target: "jupyter.comm.registerTarget('boom', () => { throw new Error('kaboom'); })",
        display_update: "console.log('before');\nawait jupyter.display(jupyter.md`one`, { display_id: 'd1' });\nawait jupyter.display(jupyter.md`two`, { display_id: 'd1', update: true });\nconsole.log('after')",
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
        echo_target: "def _echo(comm, msg):\n    comm.on_msg(lambda m: comm.send(m['content']['data'], buffers=m['buffers']))\nget_ipython().kernel.comm_manager.register_target('echo', _echo)",
        cwd: "import os; print(os.getcwd())",
        complete_setup: "zebrafish = 1",
        complete_prefix: "zebraf",
        complete_match: "zebrafish",
        incomplete: "for i in range(3):",
        inspect: "len",
        no_stdin: "try:\n    input('x')\nexcept Exception as e:\n    print(type(e).__name__)",
        no_stdin_says: "StdinNotImplementedError",
        exit: "import os; os._exit(0)",
        exit_kills_kernel: true,
        closer_target: "import json\nclosed_with = None\ndef _closer(comm, msg):\n    def _closed(m):\n        global closed_with\n        closed_with = m['content']['data']\n    comm.on_close(_closed)\nget_ipython().kernel.comm_manager.register_target('closer', _closer)",
        read_closed: "print(json.dumps(closed_with))",
        boom_target: "def _boom(comm, msg):\n    raise RuntimeError('kaboom')\nget_ipython().kernel.comm_manager.register_target('boom', _boom)",
        display_update: "from IPython.display import display, Markdown\nprint('before')\nh = display(Markdown('one'), display_id='d1')\nh.update(Markdown('two'))\nprint('after')",
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
#[async_trait::async_trait]
impl Handler for Answer {
    async fn on_input(&mut self, prompt: &str, password: bool) -> Option<String> {
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
            // Binary buffers ride along, as a widget's do.
            let s = k.session();
            let m = s
                .message("comm_msg", &json!({"comm_id": id, "data": {"ping": 1}}))
                .with_buffers(vec![vec![0u8, 1, 0xff].into()]);
            s.send(Channel::Shell, m).await.unwrap();
            let echoed = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let (ch, m) = sub.recv().await.unwrap();
                    if ch == Channel::IoPub
                        && m.msg_type() == "comm_msg"
                        && m.content["comm_id"] == id
                    {
                        return m;
                    }
                }
            })
            .await
            .expect("no echo");
            assert_eq!(echoed.content["data"], json!({"ping": 1}), "{}", l.kernel);
            let bufs: Vec<&[u8]> = echoed.buffers.iter().map(|b| b.as_ref()).collect();
            assert_eq!(bufs, [&[0u8, 1, 0xff][..]], "{}", l.kernel);
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

/// Send `msg` on shell and collect the iopub messages it causes, except
/// statuses, until the kernel is idle again. For messages that get no
/// reply, such as `comm_open`.
async fn shell_outputs(
    k: &Kernel,
    msg: ganymede_engine::protocol::Message,
) -> Vec<ganymede_engine::protocol::Message> {
    let s = k.session();
    let id = msg.header.msg_id.clone();
    let mut sub = s.subscribe();
    s.send(Channel::Shell, msg).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (ch, m) = sub.recv().await.unwrap();
            if ch != Channel::IoPub || m.parent_id() != Some(id.as_str()) {
                continue;
            }
            if m.msg_type() == "status" {
                if m.content["execution_state"] == "idle" {
                    return;
                }
            } else {
                out.push(m);
            }
        }
    })
    .await
    .expect("no idle after the message");
    out
}

/// Send executes back to back, as Run All does, and return each reply's
/// status in order.
async fn queued(k: &Kernel, cells: &[(&str, bool)]) -> Vec<String> {
    let s = k.session();
    let mut sub = s.subscribe();
    let mut ids = Vec::new();
    for (code, stop_on_error) in cells {
        let mut req = ganymede_engine::protocol::content::ExecuteRequest::new(*code);
        req.stop_on_error = *stop_on_error;
        let m = s.message("execute_request", &req);
        ids.push(m.header.msg_id.clone());
        s.send(Channel::Shell, m).await.unwrap();
    }
    let mut got = std::collections::HashMap::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        while got.len() < ids.len() {
            let (ch, m) = sub.recv().await.unwrap();
            if ch == Channel::Shell && m.msg_type() == "execute_reply" {
                if let Some(p) = m.parent_id().filter(|p| ids.iter().any(|i| i == p)) {
                    got.insert(
                        p.to_string(),
                        m.content["status"].as_str().unwrap().to_string(),
                    );
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("replies so far: {got:?}"));
    ids.iter().map(|i| got[i].clone()).collect()
}

#[test]
fn stop_on_error_aborts_queued_cells() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let statuses = queued(
                &k,
                &[(l.raise, true), (l.set_state, true), (l.answer, true)],
            )
            .await;
            assert_eq!(statuses, ["error", "aborted", "aborted"], "{}", l.kernel);
            // The aborted cell never ran.
            let r = k.execute(l.read_state).await.unwrap();
            assert_eq!(r.error().map(|e| e.0), Some(l.undefined), "{}", l.kernel);
            let statuses = queued(&k, &[(l.raise, false), (l.answer, true)]).await;
            assert_eq!(statuses, ["error", "ok"], "{}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn input_without_stdin_and_interrupt_while_prompting() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let r = k.execute(l.no_stdin).await.unwrap();
            assert_eq!(plain(&r).trim(), l.no_stdin_says, "{}", l.kernel);

            assert!(k.execute(l.set_state).await.unwrap().is_ok());
            let s = k.session();
            let mut sub = s.subscribe();
            let mut req = ganymede_engine::protocol::content::ExecuteRequest::new(l.input);
            req.allow_stdin = true;
            // An error with stop_on_error makes ipykernel abort the shell
            // requests that reach it before its "stop aborting" marker,
            // which the next cell can beat on a loaded machine (#137).
            // Queue aborts are stop_on_error_aborts_queued_cells' subject.
            req.stop_on_error = false;
            let m = s.message("execute_request", &req);
            let id = m.header.msg_id.clone();
            s.send(Channel::Shell, m).await.unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let (ch, m) = sub.recv().await.unwrap();
                    if m.parent_id() != Some(id.as_str()) {
                        continue;
                    }
                    match (ch, m.msg_type()) {
                        (Channel::Stdin, "input_request") => k.interrupt().await.unwrap(),
                        (Channel::Shell, "execute_reply") => return m,
                        _ => {}
                    }
                }
            })
            .await
            .expect("no execute_reply after interrupting a prompt");
            assert_eq!(
                reply.content["status"], "error",
                "{}: {}",
                l.kernel, reply.content
            );
            // The next prompt is answered by its own reply; state is kept.
            let mut h = Answer(vec![]);
            let opts = ExecuteOptions {
                allow_stdin: true,
                ..Default::default()
            };
            let r = k.execute_with(l.input, &opts, &mut h).await.unwrap();
            assert_eq!(plain(&r), "hi Ada\n", "{}", l.kernel);
            let r = k.execute(l.read_state).await.unwrap();
            assert!(plain(&r).contains("state"), "{}: {}", l.kernel, plain(&r));
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn runtime_exit_mid_cell() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            assert!(k.execute(l.set_state).await.unwrap().is_ok());
            let r = tokio::time::timeout(Duration::from_secs(20), k.execute(l.exit))
                .await
                .expect("no answer after the runtime exited");
            if l.exit_kills_kernel {
                // The process is the runtime: the session sees it die.
                assert!(r.is_err(), "{}", l.kernel);
                for _ in 0..100 {
                    if !k.is_alive() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                assert!(!k.is_alive(), "{}", l.kernel);
                return;
            }
            assert!(!r.unwrap().is_ok(), "{}", l.kernel);
            // The next cell runs in a fresh runtime.
            let r = k.execute(l.read_state).await.unwrap();
            assert_eq!(r.error().map(|e| e.0), Some(l.undefined), "{}", l.kernel);
            let r = k.execute(l.answer).await.unwrap();
            assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn interrupt_and_shutdown_requests_on_shell() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            // ipykernel handles it on shell too, by SIGINT to its own process
            // group, which interrupts that very handler: no reply.
            if l.language != "python" {
                let r = k
                    .shell("interrupt_request", &json!({}))
                    .await
                    .unwrap_or_else(|e| panic!("{}: interrupt_request on shell: {e}", l.kernel));
                assert_eq!(r["status"], "ok", "{}: {r}", l.kernel);
            }
            let r = k
                .shell("shutdown_request", &json!({"restart": false}))
                .await
                .unwrap_or_else(|e| panic!("{}: shutdown_request on shell: {e}", l.kernel));
            assert_eq!(r["status"], "ok", "{}: {r}", l.kernel);
            for _ in 0..100 {
                if !k.is_alive() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(!k.is_alive(), "{}: still running", l.kernel);
        });
    }
}

#[test]
fn silent_execute_publishes_only_status() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let opts = ExecuteOptions {
                silent: true,
                store_history: false,
                ..Default::default()
            };
            let r = k
                .execute_with(l.answer, &opts, &mut ganymede_engine::NoHandler)
                .await
                .unwrap();
            assert!(r.is_ok(), "{}", l.kernel);
            let types: Vec<_> = r.messages.iter().map(|m| m.msg_type()).collect();
            assert!(types.is_empty(), "{}: {types:?}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn comm_edge_cases() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let s = k.session();
            let open = |id: &str, target: &str| {
                s.message(
                    "comm_open",
                    &json!({"comm_id": id, "target_name": target, "data": {}}),
                )
            };

            // An unknown target: the kernel closes the comm.
            let out = shell_outputs(&k, open("nobody-1", "nobody")).await;
            assert!(
                out.iter()
                    .any(|m| m.msg_type() == "comm_close" && m.content["comm_id"] == "nobody-1"),
                "{}: {:?}",
                l.kernel,
                out.iter().map(|m| m.msg_type()).collect::<Vec<_>>()
            );

            // A handler that throws leaves the kernel working.
            assert!(k.execute(l.boom_target).await.unwrap().is_ok());
            let out = shell_outputs(&k, open("boom-1", "boom")).await;
            if l.language == "typescript" {
                let stderr: String = out
                    .iter()
                    .filter(|m| m.msg_type() == "stream")
                    .filter_map(|m| m.content["text"].as_str())
                    .collect();
                assert!(stderr.contains("kaboom"), "{}: {stderr:?}", l.kernel);
            }
            let r = k.execute(l.answer).await.unwrap();
            assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);

            // The frontend closes a comm: the kernel's handler gets the data
            // and the comm is gone.
            assert!(k.execute(l.closer_target).await.unwrap().is_ok());
            shell_outputs(&k, open("closer-1", "closer")).await;
            let comms = k.comm_info(None).await.unwrap();
            assert!(comms.get("closer-1").is_some(), "{}: {comms}", l.kernel);
            let close = s.message(
                "comm_close",
                &json!({"comm_id": "closer-1", "data": {"why": "frontend"}}),
            );
            shell_outputs(&k, close).await;
            let r = k.execute(l.read_closed).await.unwrap();
            let v: Value = serde_json::from_str(plain(&r).trim()).unwrap();
            assert_eq!(v, json!({"why": "frontend"}), "{}", l.kernel);
            let comms = k.comm_info(None).await.unwrap();
            assert!(comms.get("closer-1").is_none(), "{}: {comms}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn history_tail_and_search() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            k.execute("'first entry'").await.unwrap();
            k.execute("'second entry'").await.unwrap();
            let inputs = |r: &Value| -> Vec<String> {
                r["history"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{}: {r}", l.kernel))
                    .iter()
                    .map(|e| e[2].as_str().unwrap().to_string())
                    .collect()
            };
            let r = k
                .shell(
                    "history_request",
                    &json!({"hist_access_type": "tail", "n": 2, "raw": true, "output": false}),
                )
                .await
                .unwrap();
            assert_eq!(inputs(&r), ["'first entry'", "'second entry'"], "{}", l.kernel);
            let r = k
                .shell(
                    "history_request",
                    &json!({"hist_access_type": "search", "pattern": "'second*", "raw": true, "output": false, "unique": true}),
                )
                .await
                .unwrap();
            assert!(inputs(&r).contains(&"'second entry'".to_string()), "{}: {r}", l.kernel);
            assert!(!inputs(&r).contains(&"'first entry'".to_string()), "{}: {r}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn display_and_update_in_order() {
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            let r = k.execute(l.display_update).await.unwrap();
            assert!(r.is_ok(), "{}: {:?}", l.kernel, r.error());
            let seen: Vec<(String, String)> = r
                .messages
                .iter()
                .filter(|m| m.msg_type() != "execute_input")
                .map(|m| {
                    let what = match m.msg_type() {
                        "stream" => m.content["text"]
                            .as_str()
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                        _ => m.content["data"]["text/markdown"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    };
                    (m.msg_type().to_string(), what)
                })
                .collect();
            let want: Vec<(String, String)> = [
                ("stream", "before"),
                ("display_data", "one"),
                ("update_display_data", "two"),
                ("stream", "after"),
            ]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
            assert_eq!(seen, want, "{}", l.kernel);
            let update = r
                .messages
                .iter()
                .find(|m| m.msg_type() == "update_display_data")
                .unwrap();
            assert_eq!(update.content["transient"], json!({"display_id": "d1"}));
            k.shutdown().await.unwrap();
        });
    }
}

#[test]
fn parallel_starts() {
    for l in langs() {
        rt().block_on(async {
            let e = Arc::new(engine());
            let starts = (0..4).map(|_| {
                let e = e.clone();
                tokio::spawn(async move { e.start(l.kernel).await })
            });
            let kernels: Vec<Kernel> = futures::future::join_all(starts)
                .await
                .into_iter()
                .map(|r| r.unwrap().unwrap_or_else(|e| panic!("{}: {e}", l.kernel)))
                .collect();
            for k in &kernels {
                let r = k.execute(l.answer).await.unwrap();
                assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            }
            for k in kernels {
                k.shutdown().await.unwrap();
            }
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
