//! Real child processes through the subprocess provisioner and a real
//! ZeroMQ transport. The "kernels" are shell scripts that speak no
//! protocol, so these check what the host does to a process: what it
//! passes in, how it interrupts, how it escalates a shutdown, and what it
//! cleans up.
#![cfg(all(unix, feature = "subprocess", feature = "zmq"))]

use ganymede_kernel_host::zmq::ZmqTransport;
use ganymede_kernel_host::{
    KernelSession, Kernelspec, LaunchOptions, LaunchedKernel, Provisioner, SubprocessProvisioner,
};
use std::path::Path;
use std::time::Duration;

fn spec(script: &str) -> Kernelspec {
    let mut spec: Kernelspec = serde_json::from_value(serde_json::json!({
        "argv": ["sh", "-c", script, "kernel", "{connection_file}"],
        "display_name": "sh",
        "language": "sh",
        "env": {"FROM_SPEC": "spec"},
    }))
    .unwrap();
    spec.name = "sh".into();
    spec
}

async fn launch(dir: &Path, script: &str, opts: LaunchOptions) -> LaunchedKernel {
    let prov = SubprocessProvisioner {
        runtime_dir: dir.join("runtime"),
        ip: "127.0.0.1".into(),
    };
    prov.launch(
        &spec(script),
        LaunchOptions {
            quiet: true,
            ..opts
        },
    )
    .await
    .unwrap()
}

async fn session(k: &LaunchedKernel) -> KernelSession {
    let transport = ZmqTransport::connect(k.connection.as_ref().unwrap())
        .await
        .unwrap();
    KernelSession::start(transport, Some(k))
}

/// Wait for `path` to contain `want`.
async fn file_says(path: &Path, want: &str) -> String {
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(path) {
            if s.contains(want) {
                return s;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "{} never said {want:?}: {:?}",
        path.display(),
        std::fs::read_to_string(path)
    );
}

#[tokio::test]
async fn kernel_gets_its_connection_file_env_and_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    let out = dir.path().join("out");
    let script = format!(
        "{{ pwd -P; echo \"$1\"; echo \"$FROM_SPEC $FROM_LAUNCH $JPY_SESSION_NAME $JPY_PARENT_PID\"; \
         cat \"$1\"; echo; echo done; }} > '{}'; sleep 30",
        out.display()
    );
    let opts = LaunchOptions {
        cwd: Some(cwd.clone()),
        env: vec![("FROM_LAUNCH".into(), "launch".into())],
        kernel_id: Some("k-123".into()),
        ..Default::default()
    };
    let k = launch(dir.path(), &script, opts).await;
    let text = file_says(&out, "done").await;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(Path::new(lines[0]), cwd.canonicalize().unwrap());
    let file = k.connection_file.clone().unwrap();
    assert_eq!(Path::new(lines[1]), file);
    assert!(file.ends_with("kernel-k-123.json"));
    assert_eq!(
        lines[2],
        format!("spec launch k-123 {}", std::process::id())
    );
    // The kernel reads the same ports the host connects to.
    let json = &text[text.find('{').unwrap()..text.rfind('}').unwrap() + 1];
    let info: ganymede_kernel_host::protocol::ConnectionInfo = serde_json::from_str(json).unwrap();
    assert_eq!(Some(info), k.connection);

    k.process.kill().await.unwrap();
    k.process.wait().await;
    drop(k);
    assert!(!file.exists(), "connection file removed with the kernel");
}

#[tokio::test]
async fn signal_interrupt_reaches_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    // The shell and the child it waits on both see SIGINT. (A child run
    // with `&` would not: a non-interactive shell starts it ignoring
    // SIGINT.)
    let script = format!(
        "trap 'echo kernel >> {o}' INT; sh -c \"trap 'echo child >> {o}; exit 0' INT; \
         echo ready >> {o}; while :; do sleep 0.05; done\"; \
         while :; do sleep 0.05; done",
        o = out.display()
    );
    let k = launch(dir.path(), &script, Default::default()).await;
    let s = session(&k).await;
    file_says(&out, "ready").await;
    s.interrupt().await.unwrap();
    let text = file_says(&out, "child").await;
    let text = if text.contains("kernel") {
        text
    } else {
        file_says(&out, "kernel").await
    };
    assert!(text.contains("kernel") && text.contains("child"), "{text}");
    assert!(k.process.is_alive(), "an interrupt is not a kill");
    k.process.kill().await.unwrap();
}

/// SIGKILL follows SIGTERM after half of this. The shell runs its TERM
/// trap only once it is scheduled again, which on a loaded macOS runner
/// took more than 200 ms (#212), so give it a full second.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[tokio::test]
async fn shutdown_escalates_to_sigterm_then_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    // Never answers shutdown_request, ignores SIGTERM.
    let script = format!(
        "trap 'echo term >> {o}' TERM; echo ready >> {o}; while :; do sleep 0.05; done",
        o = out.display()
    );
    let k = launch(dir.path(), &script, Default::default()).await;
    let s = session(&k).await;
    file_says(&out, "ready").await;
    let start = std::time::Instant::now();
    s.shutdown(false, SHUTDOWN_GRACE).await.unwrap();
    // Request, wait and SIGTERM wait take 2 x grace before SIGKILL.
    assert!(start.elapsed() < 2 * SHUTDOWN_GRACE + Duration::from_secs(3));
    assert!(!k.process.is_alive(), "killed in the end");
    assert!(!s.is_alive());
    file_says(&out, "term").await;

    // A kernel that exits on SIGTERM is not killed.
    let out2 = dir.path().join("out2");
    let script = format!(
        "trap 'echo term >> {o}; exit 3' TERM; echo ready >> {o}; while :; do sleep 0.05; done",
        o = out2.display()
    );
    let k = launch(dir.path(), &script, Default::default()).await;
    let s = session(&k).await;
    file_says(&out2, "ready").await;
    s.shutdown(false, SHUTDOWN_GRACE).await.unwrap();
    assert_eq!(k.process.wait().await, Some(3), "exited by its own trap");
}

#[tokio::test]
async fn a_kernel_that_exits_is_reported_dead() {
    let dir = tempfile::tempdir().unwrap();
    let k = launch(dir.path(), "sleep 0.3; exit 7", Default::default()).await;
    let s = session(&k).await;
    tokio::time::timeout(Duration::from_secs(5), s.dead())
        .await
        .expect("death not noticed");
    assert_eq!(k.process.wait().await, Some(7));
    assert!(matches!(
        s.interrupt().await,
        Err(ganymede_kernel_host::Error::KernelDied)
    ));
}
