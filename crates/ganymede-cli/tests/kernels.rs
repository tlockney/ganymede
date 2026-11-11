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
    /// Three lines; the second sets `second`, the third evaluates to 42.
    debug_cell: &'static str,
    /// Code that hands the kernel's own runtime binding something that is
    /// not JSON, where the language exposes one.
    garbage_binding: Option<&'static str>,
    /// Publishes `GANYMEDE_PARAMS` as the named result `got`, by hand: a
    /// raw `application/vnd.ganymede.result+json` output.
    params_result: &'static str,
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
        debug_cell: "const first: number = 20;\nconst second = first + 1;\nsecond * 2",
        garbage_binding: Some("__cdpk_send('not json'); 1"),
        params_result: "await jupyter.display({ 'application/vnd.ganymede.result+json': { name: 'got', value: JSON.parse(process.env.GANYMEDE_PARAMS ?? '{}') } }, { raw: true })",
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
        debug_cell: "first = 20\nsecond = first + 1\nsecond * 2",
        garbage_binding: None,
        params_result: "import os, json\nfrom IPython.display import display\ndisplay({'application/vnd.ganymede.result+json': {'name': 'got', 'value': json.loads(os.environ.get('GANYMEDE_PARAMS', '{}'))}}, raw=True)",
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
    ganymede_nbformat::strip_ansi(&r.text())
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
            // The kernels' own output goes to stderr, each line marked with
            // its kernel's id, so a kernel that dies leaves its last words
            // in the log (#327).
            let e = Arc::new(Engine::builder().prefix_output(true).build());
            let starts = (0..4).map(|_| {
                let e = e.clone();
                tokio::spawn(async move { e.start(l.kernel).await })
            });
            let kernels: Vec<Kernel> = futures::future::join_all(starts)
                .await
                .into_iter()
                .map(|r| r.unwrap().unwrap_or_else(|e| panic!("{}: {e}", l.kernel)))
                .collect();
            for (i, k) in kernels.iter().enumerate() {
                let r = k.execute(l.answer).await.unwrap_or_else(|e| {
                    // `kernel-<id>.json`: its output above carries
                    // `[kernel <first 8 of id>]`.
                    let file = k.connection_file().unwrap_or_default();
                    panic!(
                        "{} #{i} ({}, alive: {}): {e}; its output is in stderr above",
                        l.kernel,
                        file.display(),
                        k.is_alive()
                    )
                });
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
        assert_eq!(
            ganymede_nbformat::strip_ansi(&stdout(&out)),
            "42\nout\n",
            "{}",
            l.kernel
        );

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
fn cli_execute_passes_params_and_prints_results() {
    for l in langs() {
        let dir = tempfile::tempdir().unwrap();
        let nb = dir.path().join("p.ipynb");
        let doc = json!({
            "nbformat": 4, "nbformat_minor": 5,
            "metadata": { "kernelspec": { "name": l.kernel, "display_name": l.kernel, "language": l.language } },
            "cells": [{ "cell_type": "code", "id": "a", "metadata": {}, "source": l.params_result, "outputs": [], "execution_count": null }],
        });
        std::fs::write(&nb, doc.to_string()).unwrap();
        let out = run_ok(
            ganymede(&[
                "execute",
                "-q",
                "--results",
                "-p",
                "counter=2nd Ave",
                "-p",
                "days=7",
            ])
            .arg(&nb),
        );
        let line: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        assert_eq!(
            line["results"],
            json!({ "got": { "counter": "2nd Ave", "days": 7 } }),
            "{}",
            l.kernel
        );

        // Without -p the kernel gets `{}`, not a parent run's parameters.
        let out = run_ok(
            ganymede(&["execute", "-q", "--results"])
                .env("GANYMEDE_PARAMS", r#"{"leaked":1}"#)
                .arg(&nb),
        );
        let line: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
        assert_eq!(line["results"], json!({ "got": {} }), "{}", l.kernel);
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
        let text = ganymede_nbformat::strip_ansi(&stdout(&out))
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
        assert!(
            ganymede_nbformat::strip_ansi(&stdout(&out)).contains("state"),
            "{}",
            l.kernel
        );

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

/// A `debug_request` on control, as JupyterLab's debugger sends it: done
/// once the reply and the request's idle status have both arrived.
async fn debug(k: &Kernel, command: &str, arguments: Value) -> Value {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let s = k.session();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let m = s.message(
        "debug_request",
        &json!({"type": "request", "seq": seq, "command": command, "arguments": arguments}),
    );
    let e = s
        .request(Channel::Control, m, true, Duration::from_secs(30))
        .await
        .unwrap_or_else(|e| panic!("debug_request {command}: {e}"));
    assert_eq!(e.reply.msg_type(), "debug_reply");
    e.reply.content
}

/// JupyterLab's debugger over the control channel: a breakpoint stops a
/// cell, `debug_event`s arrive on iopub, and the cell finishes on
/// `continue`. For kernels whose `kernel_info_reply` offers a debugger.
#[test]
fn debugger_stops_at_a_breakpoint() {
    for l in langs() {
        rt().block_on(async {
            let k = Arc::new(engine().start(l.kernel).await.unwrap());
            // `debugger: true`, or from protocol 5.5 (ipykernel 7) a
            // `supported_features` entry.
            let info = k.info_json();
            let features = info["supported_features"].as_array().cloned().unwrap_or_default();
            if info["debugger"] != true && !features.contains(&json!("debugger")) {
                eprintln!("skipping: {} reports no debugger", l.kernel);
                k.shutdown().await.unwrap();
                return;
            }
            let info = debug(&k, "debugInfo", json!({})).await;
            assert_eq!(info["success"], true, "{}: {info}", l.kernel);
            let info = info["body"].clone();
            let init = debug(
                &k,
                "initialize",
                json!({"clientID": "ganymede", "adapterID": "", "pathFormat": "path",
                       "linesStartAt1": true, "columnsStartAt1": true}),
            )
            .await;
            assert_eq!(init["success"], true, "{}: {init}", l.kernel);
            let mut sub = k.session().subscribe();
            let attach = debug(&k, "attach", json!({})).await;
            assert_eq!(attach["success"], true, "{}: {attach}", l.kernel);
            let dumped = debug(&k, "dumpCell", json!({"code": l.debug_cell})).await;
            let path = dumped["body"]["sourcePath"].as_str().unwrap().to_string();
            let prefix = info["tmpFilePrefix"].as_str().unwrap();
            assert!(path.starts_with(prefix), "{}: {path}", l.kernel);
            let set = debug(
                &k,
                "setBreakpoints",
                json!({"source": {"path": path}, "breakpoints": [{"line": 2}], "sourceModified": false}),
            )
            .await;
            assert_eq!(set["body"]["breakpoints"][0]["verified"], true, "{}: {set}", l.kernel);
            debug(&k, "configurationDone", json!({})).await;

            let k2 = k.clone();
            let code = l.debug_cell;
            let cell = tokio::spawn(async move { k2.execute(code).await });
            let stopped = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let (ch, m) = sub.recv().await.unwrap();
                    if ch == Channel::IoPub
                        && m.msg_type() == "debug_event"
                        && m.content["event"] == "stopped"
                    {
                        return m.content;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{}: no stopped event", l.kernel));
            let thread = stopped["body"]["threadId"].clone();
            let frames = debug(&k, "stackTrace", json!({"threadId": thread})).await;
            let top = &frames["body"]["stackFrames"][0];
            assert_eq!(top["line"], 2, "{}: {frames}", l.kernel);
            assert_eq!(top["source"]["path"], path.as_str(), "{}", l.kernel);
            let go = debug(&k, "continue", json!({"threadId": thread})).await;
            assert_eq!(go["success"], true, "{}: {go}", l.kernel);
            let r = tokio::time::timeout(Duration::from_secs(30), cell)
                .await
                .unwrap_or_else(|_| panic!("{}: cell never finished", l.kernel))
                .unwrap()
                .unwrap();
            assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            let bye = debug(
                &k,
                "disconnect",
                json!({"restart": false, "terminateDebuggee": false}),
            )
            .await;
            assert_eq!(bye["success"], true, "{}: {bye}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

/// Before answering a prompt, sends the kernel's stdin socket a frame set
/// whose header is not JSON, from a peer of its own.
struct Rogue(zeromq::DealerSocket);
#[async_trait::async_trait]
impl Handler for Rogue {
    async fn on_input(&mut self, _prompt: &str, _password: bool) -> Option<String> {
        use zeromq::SocketSend;
        let frames: Vec<bytes::Bytes> = [&b"<IDS|MSG>"[..], b"", b"not json", b"{}", b"{}", b"{}"]
            .into_iter()
            .map(bytes::Bytes::copy_from_slice)
            .collect();
        self.0
            .send(zeromq::ZmqMessage::try_from(frames).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        Some("Ada".into())
    }
}

/// A malformed frame set on stdin while a prompt waits is dropped and the
/// real `input_reply` still answers it; cdp-kernel's runtime binding fed
/// something that is not JSON leaves the kernel working. From cdp-kernel's
/// own e2e (`malformed_stdin_message`, `garbage_binding_payload`).
#[test]
fn malformed_stdin_and_garbage_payloads_are_tolerated() {
    use zeromq::Socket;
    for l in langs() {
        rt().block_on(async {
            let k = engine().start(l.kernel).await.unwrap();
            if let Some(code) = l.garbage_binding {
                let r = k.execute(code).await.unwrap();
                assert!(r.is_ok(), "{}: {:?}", l.kernel, r.reply);
                let r = k.execute(l.answer).await.unwrap();
                assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            }
            let info = k.connection_info().expect("a ZeroMQ kernel");
            let mut rogue = zeromq::DealerSocket::new();
            rogue.connect(&info.stdin_endpoint()).await.unwrap();
            let opts = ExecuteOptions {
                allow_stdin: true,
                ..Default::default()
            };
            let mut h = Rogue(rogue);
            let r = tokio::time::timeout(
                Duration::from_secs(30),
                k.execute_with(l.input, &opts, &mut h),
            )
            .await
            .unwrap_or_else(|_| panic!("{}: the prompt was never answered", l.kernel))
            .unwrap();
            assert_eq!(plain(&r), "hi Ada\n", "{}", l.kernel);
            let _ = h.0.close().await;
            let r = k.execute(l.answer).await.unwrap();
            assert_eq!(plain(&r).trim(), "42", "{}", l.kernel);
            k.shutdown().await.unwrap();
        });
    }
}

/// `ganymede execute` answers its kernels' host calls (docs/host-api.md)
/// under the config's `[host]` grants, with `--grant` in place of a key.
/// cdp-kernel only: ipykernel has no host client.
#[test]
fn execute_answers_host_calls_under_its_grants() {
    if !have_kernel("cdp-node") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = root.join(".config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    let notebook = |source: &str| {
        json!({
            "cells": [{"cell_type": "code", "id": "c1", "metadata": {}, "source": source,
                       "outputs": [], "execution_count": null}],
            "metadata": {"kernelspec": {"name": "cdp-node", "display_name": "Node", "language": "typescript"}},
            "nbformat": 4, "nbformat_minor": 5
        })
        .to_string()
    };
    std::fs::write(root.join("docs/a.ipynb"), notebook("1 + 1")).unwrap();
    std::fs::write(root.join("secret.ipynb"), notebook("2 + 2")).unwrap();
    std::fs::write(
        root.join("main.ipynb"),
        notebook(
            r#"const h = await jupyter.host();
const out: Record<string, unknown> = { notebook: h?.notebook, run: h?.has("notebooks.run"), edit: h?.has("self.add") };
const code = (p: Promise<unknown>) => p.then(() => "ok", (e) => e.code);
out.self = await code(h!.call("notebooks.read", { path: "main.ipynb" }));
out.docs = await code(h!.call("notebooks.read", { path: "docs/a.ipynb" }));
out.secret = await code(h!.call("notebooks.read", { path: "secret.ipynb" }));
if (out.run) out.ran = (await h!.call("notebooks.run", { path: "docs/a.ipynb" })).status;
console.log("HOST " + JSON.stringify(out));"#,
        ),
    )
    .unwrap();
    // A data dir of its own, for the file ids `ganymede grant` keeps, with
    // the kernelspec in it.
    let data = root.join(".data");
    let spec = engine().kernelspec("cdp-node").unwrap().resource_dir;
    std::fs::create_dir_all(data.join("kernels/cdp-node")).unwrap();
    for f in std::fs::read_dir(&spec).unwrap() {
        let f = f.unwrap();
        if f.file_type().unwrap().is_file() {
            std::fs::copy(f.path(), data.join("kernels/cdp-node").join(f.file_name())).unwrap();
        }
    }
    let report = |grants: &[&str]| -> Value {
        let mut args = vec!["execute", "main.ipynb", "--output", "-"];
        args.extend(grants);
        let mut cmd = ganymede(&args);
        cmd.current_dir(root)
            .env("JUPYTER_CONFIG_DIR", &config)
            .env("JUPYTER_DATA_DIR", &data);
        let out = run_ok(cmd);
        let nb: Value = serde_json::from_slice(&out.stdout).unwrap();
        let text = nb["cells"][0]["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| o["text"].as_str().or_else(|| o["text"][0].as_str()))
            .collect::<String>();
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix("HOST "))
            .unwrap_or_else(|| panic!("no host report in {nb}"));
        serde_json::from_str(line).unwrap()
    };
    // The defaults: read everything, run nothing, and no self.* calls.
    assert_eq!(
        report(&[]),
        json!({"notebook": "main.ipynb", "run": false, "edit": false,
               "self": "ok", "docs": "ok", "secret": "ok"})
    );
    // --grant replaces a key: reads narrow to docs/ (and the notebook
    // itself), and docs/ may be run.
    assert_eq!(
        report(&["--grant", "read=docs", "--grant", "run=docs"]),
        json!({"notebook": "main.ipynb", "run": true, "edit": false,
               "self": "ok", "docs": "ok", "secret": "denied", "ran": "ok"})
    );
    // So does the config file's [host].
    std::fs::write(config.join("ganymede.toml"), "[host]\nread = [\"docs\"]\n").unwrap();
    assert_eq!(report(&[])["secret"], "denied");
    std::fs::remove_file(config.join("ganymede.toml")).unwrap();
    // A notebook's own grant, from `ganymede grant`, applies as on a
    // server, and still gives no self.* calls.
    let mut grant = ganymede(&["grant", "main.ipynb", "read=docs", "edit_self=true"]);
    grant
        .current_dir(root)
        .env("JUPYTER_CONFIG_DIR", &config)
        .env("JUPYTER_DATA_DIR", &data);
    run_ok(grant);
    let own = report(&["--grant", "read="]);
    assert_eq!(
        (&own["secret"], &own["edit"]),
        (&json!("denied"), &json!(false))
    );
    // A config file that doesn't parse leaves the run without the host API.
    std::fs::write(config.join("ganymede.toml"), "[host\n").unwrap();
    let mut cmd = ganymede(&["execute", "main.ipynb", "--output", "-"]);
    cmd.current_dir(root)
        .env("JUPYTER_CONFIG_DIR", &config)
        .env("JUPYTER_DATA_DIR", &data);
    let out = cmd.output().unwrap();
    assert!(
        stderr(&out).contains("no host API for this run"),
        "{}",
        stderr(&out)
    );
}
