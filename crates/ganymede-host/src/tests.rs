use super::*;
use ganymede_kernel_host::in_process_pair;
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_protocol::content::ExecuteRequest;
use tokio::sync::broadcast;

const T: Duration = Duration::from_secs(5);

/// Answers `echo` with its params, `slow` after a while, and nothing
/// else.
struct Echo;

#[async_trait]
impl Responder for Echo {
    async fn hello(&self, _caller: &Caller) -> Hello {
        Hello {
            notebook: Some("journal/a.ipynb".into()),
            methods: vec!["echo".into(), "slow".into()],
        }
    }
    async fn call(
        &self,
        _caller: &Caller,
        method: &str,
        params: Value,
    ) -> Result<Value, HostError> {
        match method {
            "echo" => Ok(params),
            "slow" => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(json!("late"))
            }
            "secret" => Err(HostError::denied("not granted")),
            _ => Err(HostError::unavailable(format!("no method {method}"))),
        }
    }
}

async fn boot(host: bool) -> KernelSession {
    let (transport, kernel) = in_process_pair(256);
    if host {
        FakeKernel::spawn_with_host(kernel, ReplyOrder::ReplyThenIdle);
    } else {
        FakeKernel::spawn(kernel, ReplyOrder::ReplyThenIdle);
    }
    let s = KernelSession::start(transport, None);
    s.nudge(T).await.unwrap();
    s
}

fn link(s: &KernelSession) -> HostLink {
    attach(
        s,
        Caller {
            kernel_id: "k".into(),
        },
        Arc::new(Echo),
        "9.9.9",
    )
}

/// Run a cell, and return the stdout it printed.
async fn run(s: &KernelSession, code: &str) -> String {
    let e = s.execute(ExecuteRequest::new(code), T).await.unwrap();
    stdout(&e.outputs)
}

fn stdout(outputs: &[Message]) -> String {
    outputs
        .iter()
        .filter(|m| m.msg_type() == "stream")
        .map(|m| m.content["text"].as_str().unwrap_or_default())
        .collect()
}

/// The next stdout the kernel prints, from any cell.
async fn next_print(rx: &mut broadcast::Receiver<(Channel, Message)>) -> String {
    tokio::time::timeout(T, async {
        loop {
            let (channel, m) = rx.recv().await.unwrap();
            if channel == Channel::IoPub && m.msg_type() == "stream" {
                return m.content["text"].as_str().unwrap_or_default().to_string();
            }
        }
    })
    .await
    .expect("a print")
}

/// Wait until the kernel has the host comm.
async fn opened(s: &KernelSession) -> Value {
    for _ in 0..200 {
        let out = run(s, "host-hello").await;
        if out != "no host\n" {
            return serde_json::from_str(out.trim()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the host comm never opened");
}

#[tokio::test]
async fn opens_the_comm_with_hello() {
    let s = boot(true).await;
    let _link = link(&s);
    let hello = opened(&s).await;
    assert_eq!(
        hello,
        json!({"protocol": 1, "server": "9.9.9", "notebook": "journal/a.ipynb",
               "methods": ["echo", "slow"]})
    );
}

#[tokio::test]
async fn answers_calls_with_results_and_errors() {
    let s = boot(true).await;
    let _link = link(&s);
    opened(&s).await;
    let mut rx = s.subscribe();
    run(&s, r#"host echo {"a":1}"#).await;
    assert_eq!(
        next_print(&mut rx).await,
        "host 0: {\"id\":0,\"result\":{\"a\":1}}\n"
    );
    run(&s, "host secret {}").await;
    assert_eq!(
        next_print(&mut rx).await,
        "host 1: {\"id\":1,\"error\":{\"code\":\"denied\",\"message\":\"not granted\"}}\n"
    );
    run(&s, "host nope {}").await;
    let out = next_print(&mut rx).await;
    assert!(out.contains("\"unavailable\""), "{out}");
    run(&s, "host hello {}").await;
    let out = next_print(&mut rx).await;
    assert!(out.contains("\"protocol\":1"), "{out}");
}

#[tokio::test]
async fn a_cancelled_call_gets_no_reply() {
    let s = boot(true).await;
    let _link = link(&s);
    opened(&s).await;
    let mut rx = s.subscribe();
    run(&s, "host slow {}").await;
    run(&s, "host-cancel 0").await;
    run(&s, r#"host echo {"b":2}"#).await;
    // The next answer is the echo's: the slow call was dropped.
    assert_eq!(
        next_print(&mut rx).await,
        "host 1: {\"id\":1,\"result\":{\"b\":2}}\n"
    );
}

#[tokio::test]
async fn no_comm_without_subshells() {
    let s = boot(false).await;
    let mut rx = s.subscribe();
    let _link = link(&s);
    // Nothing the host sends reaches the kernel: no subshell request, no
    // open.
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok((_, m)) = rx.try_recv() {
        assert!(
            m.parent_header
                .as_ref()
                .is_none_or(|p| !p.session.starts_with("ganymede-host-")),
            "{m:?}"
        );
    }
}

#[tokio::test]
async fn a_second_open_is_turned_away() {
    let s = boot(true).await;
    let _link = link(&s);
    opened(&s).await;
    // The fake swaps its comm for the one it opens; the host closes that
    // one ("taken"), so the fake is left with none.
    run(&s, "host-open").await;
    for _ in 0..200 {
        if run(&s, "host-hello").await == "no host\n" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the second open was not closed");
}

#[tokio::test]
async fn a_late_client_opens_the_comm_itself() {
    let s = boot(true).await;
    let _link = link(&s);
    opened(&s).await;
    run(&s, "host-close").await;
    assert_eq!(run(&s, "host-hello").await, "no host\n");
    // Opened from the kernel: the host takes it and sends hello on it.
    run(&s, "host-open").await;
    let hello = opened(&s).await;
    for _ in 0..200 {
        let out = run(&s, "host-hello").await;
        if out != "null\n" {
            let hello: Value = serde_json::from_str(out.trim()).unwrap();
            assert_eq!(hello["notebook"], "journal/a.ipynb");
            let mut rx = s.subscribe();
            run(&s, r#"host echo {"d":4}"#).await;
            assert_eq!(
                next_print(&mut rx).await,
                "host 0: {\"id\":0,\"result\":{\"d\":4}}\n"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no hello on the kernel's comm: {hello}");
}

#[tokio::test]
async fn host_traffic_is_recognised() {
    let s = boot(true).await;
    let link = link(&s);
    opened(&s).await;
    let mut rx = s.subscribe();
    run(&s, r#"host echo {"c":3}"#).await;
    let mut call = None;
    let mut answer = None;
    tokio::time::timeout(T, async {
        while call.is_none() || answer.is_none() {
            let (channel, m) = rx.recv().await.unwrap();
            match m.msg_type() {
                "comm_msg" => call = Some((channel, m)),
                "stream" => answer = Some((channel, m)),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let (channel, call) = call.unwrap();
    assert!(
        link.is_host_message(channel, &call),
        "the call is the host's"
    );
    let (channel, answer) = answer.unwrap();
    assert!(
        !link.is_host_message(channel, &answer),
        "the cell's print is not"
    );
    // A status for a message the host sent is the host's.
    let ours = Message::new("comm_msg", &link.shared.tag, "ganymede", &json!({}));
    let status = Message::reply_to(&ours, "status", &json!({"execution_state": "idle"}));
    assert!(link.is_host_message(Channel::IoPub, &status));
    // Another comm's traffic is not.
    let other = Message::new(
        "comm_msg",
        "x",
        "x",
        &json!({"comm_id": "other", "data": {}}),
    );
    assert!(!link.is_host_message(Channel::IoPub, &other));
}

#[tokio::test]
async fn a_kernel_open_is_recorded_in_forwarding_order() {
    let s = boot(true).await;
    let link = link(&s);
    opened(&s).await;
    // Seen by the forwarder before the driver: the open and the call
    // right after it are both the host's.
    let open = Message::new(
        "comm_open",
        "k",
        "kernel",
        &json!({"comm_id": "late", "target_name": TARGET, "data": {}}),
    );
    assert!(link.is_host_message(Channel::IoPub, &open));
    let call = Message::new(
        "comm_msg",
        "k",
        "kernel",
        &json!({"comm_id": "late", "data": {"id": 0, "method": "echo"}}),
    );
    assert!(link.is_host_message(Channel::IoPub, &call));
    let close = Message::new(
        "comm_close",
        "k",
        "kernel",
        &json!({"comm_id": "late", "data": {}}),
    );
    assert!(link.is_host_message(Channel::IoPub, &close));
}

#[tokio::test]
async fn without_a_driver_a_kernel_open_goes_to_clients() {
    let s = boot(false).await;
    let link = link(&s);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let open = Message::new(
        "comm_open",
        "k",
        "kernel",
        &json!({"comm_id": "late", "target_name": TARGET, "data": {}}),
    );
    assert!(!link.is_host_message(Channel::IoPub, &open));
}

/// Answers `wait` never, and notes when its call is dropped.
struct Waits(Arc<std::sync::atomic::AtomicBool>);

struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl Responder for Waits {
    async fn hello(&self, _caller: &Caller) -> Hello {
        Hello {
            notebook: None,
            methods: vec!["wait".into()],
        }
    }
    async fn call(&self, _: &Caller, _: &str, _: Value) -> Result<Value, HostError> {
        let _guard = SetOnDrop(self.0.clone());
        std::future::pending::<()>().await;
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn dropping_the_link_ends_its_calls() {
    let s = boot(true).await;
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let link = attach(
        &s,
        Caller {
            kernel_id: "k".into(),
        },
        Arc::new(Waits(dropped.clone())),
        "1",
    );
    opened(&s).await;
    run(&s, "host wait {}").await;
    // The call is in flight.
    for _ in 0..200 {
        if !link.shared.calls.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!link.shared.calls.lock().unwrap().is_empty());
    drop(link);
    for _ in 0..200 {
        if dropped.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the call outlived the link");
}
