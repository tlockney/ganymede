//! The per-kernel state machine.
//!
//! What it knows that a plain message pipe does not:
//! - The startup nudge: `kernel_info_request` on shell and control until
//!   both answer and at least one iopub message has arrived, because PUB/SUB
//!   drops what was published before the subscriber joined.
//! - Completion needs two signals: the reply on shell, and an iopub
//!   `status: idle` whose parent is the request. They arrive in either order
//!   on sockets with no ordering guarantee between them.
//! - Replies are routed to the requester by `parent_header.msg_id`; iopub
//!   is fanned out to every client.
//! - Interrupt follows the kernelspec: a signal to the process, or an
//!   `interrupt_request` on control.

use crate::process::{KernelProcess, LaunchedKernel};
use crate::transport::{KernelTransport, TransportSink};
use crate::{Error, InterruptMode, Result};
use ganymede_protocol::content::{self, ExecutionState};
use ganymede_protocol::{Channel, Message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, watch};

/// One request's progress until it is complete.
struct Pending {
    want_idle: bool,
    reply: Option<Message>,
    idle_seen: bool,
    outputs: Vec<Message>,
    done: Option<oneshot::Sender<Execution>>,
}

impl Pending {
    fn complete(&self) -> bool {
        self.reply.is_some() && (self.idle_seen || !self.want_idle)
    }
}

/// A finished request: its reply and the iopub messages it produced,
/// excluding `status`.
#[derive(Debug, Clone)]
pub struct Execution {
    pub reply: Message,
    pub outputs: Vec<Message>,
}

impl Execution {
    pub fn reply_status(&self) -> Option<content::ReplyStatus> {
        self.reply
            .content
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok())
    }
}

struct Inner {
    sink: Arc<dyn TransportSink>,
    clients: broadcast::Sender<(Channel, Message)>,
    pending: Mutex<HashMap<String, Pending>>,
    state: watch::Sender<ExecutionState>,
    iopub_seen: AtomicBool,
    alive: AtomicBool,
    session_id: String,
    username: String,
    process: Option<Arc<dyn KernelProcess>>,
    interrupt_mode: InterruptMode,
    kernel_info: Mutex<Option<content::KernelInfoReply>>,
}

/// Handle to a running kernel. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct KernelSession {
    inner: Arc<Inner>,
}

impl KernelSession {
    /// Drive `transport`. `launched` supplies the process handle and
    /// interrupt mode when the host owns the process.
    pub fn start(transport: KernelTransport, launched: Option<&LaunchedKernel>) -> Self {
        let (clients, _) = broadcast::channel(1024);
        let (state, _) = watch::channel(ExecutionState::Starting);
        let inner = Arc::new(Inner {
            sink: transport.sink,
            clients,
            pending: Mutex::new(HashMap::new()),
            state,
            iopub_seen: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            session_id: uuid::Uuid::new_v4().to_string(),
            username: whoami(),
            process: launched.map(|l| l.process.clone()),
            interrupt_mode: launched
                .map(|l| l.interrupt_mode)
                .unwrap_or(InterruptMode::Message),
            kernel_info: Mutex::new(None),
        });
        tokio::spawn(route(inner.clone(), transport.incoming));
        if let Some(p) = inner.process.clone() {
            let inner = inner.clone();
            tokio::spawn(async move {
                p.wait().await;
                inner.mark_dead();
            });
        }
        KernelSession { inner }
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    pub fn execution_state(&self) -> ExecutionState {
        *self.inner.state.borrow()
    }

    pub fn watch_state(&self) -> watch::Receiver<ExecutionState> {
        self.inner.state.subscribe()
    }

    /// The last `kernel_info_reply`, after [`KernelSession::nudge`].
    pub fn kernel_info(&self) -> Option<content::KernelInfoReply> {
        self.inner.kernel_info.lock().unwrap().clone()
    }

    /// Every message from the kernel, on every channel. Each client gets
    /// its own receiver; slow clients lose messages rather than stall the
    /// kernel.
    pub fn subscribe(&self) -> broadcast::Receiver<(Channel, Message)> {
        self.inner.clients.subscribe()
    }

    /// Build a request message in this session's identity.
    pub fn message<C: serde::Serialize>(&self, msg_type: &str, content: &C) -> Message {
        Message::new(
            msg_type,
            &self.inner.session_id,
            &self.inner.username,
            content,
        )
    }

    /// Send without waiting for anything.
    pub async fn send(&self, channel: Channel, msg: Message) -> Result<()> {
        self.ensure_alive()?;
        self.inner.sink.send(channel, msg).await
    }

    /// Send and wait for the reply (and, when `want_idle`, the matching
    /// idle status). Times out with [`Error::Timeout`].
    pub async fn request(
        &self,
        channel: Channel,
        msg: Message,
        want_idle: bool,
        timeout: Duration,
    ) -> Result<Execution> {
        self.ensure_alive()?;
        let (tx, rx) = oneshot::channel();
        let id = msg.header.msg_id.clone();
        self.inner.pending.lock().unwrap().insert(
            id.clone(),
            Pending {
                want_idle,
                reply: None,
                idle_seen: false,
                outputs: Vec::new(),
                done: Some(tx),
            },
        );
        if let Err(e) = self.inner.sink.send(channel, msg).await {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(exec)) => Ok(exec),
            Ok(Err(_)) => Err(Error::KernelDied),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(Error::Timeout("reply"))
            }
        }
    }

    /// The startup handshake. Returns the kernel's info once shell and
    /// control both answer and iopub has delivered at least one message.
    pub async fn nudge(&self, timeout: Duration) -> Result<content::KernelInfoReply> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut shell_info: Option<content::KernelInfoReply> = None;
        let mut control_ok = false;
        while tokio::time::Instant::now() < deadline {
            let step = Duration::from_millis(500);
            let req = content::KernelInfoRequest::default();
            if shell_info.is_none() {
                let m = self.message("kernel_info_request", &req);
                if let Ok(exec) = self.request(Channel::Shell, m, false, step).await {
                    shell_info = exec.reply.content_as().ok();
                }
            }
            if !control_ok {
                let m = self.message("kernel_info_request", &req);
                control_ok = self.request(Channel::Control, m, false, step).await.is_ok();
            }
            if let (Some(info), true, true) = (
                &shell_info,
                control_ok,
                self.inner.iopub_seen.load(Ordering::SeqCst),
            ) {
                *self.inner.kernel_info.lock().unwrap() = Some(info.clone());
                return Ok(info.clone());
            }
            if !self.is_alive() {
                return Err(Error::KernelDied);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(Error::Timeout(
            "kernel_info_reply on shell, control, and an iopub message",
        ))
    }

    /// Execute code and collect its outputs. Completes on reply plus idle.
    pub async fn execute(
        &self,
        req: content::ExecuteRequest,
        timeout: Duration,
    ) -> Result<Execution> {
        let m = self.message("execute_request", &req);
        self.request(Channel::Shell, m, true, timeout).await
    }

    /// Answer a kernel's `input_request`.
    pub async fn reply_input(&self, input_request: &Message, value: &str) -> Result<()> {
        let mut m = self.message(
            "input_reply",
            &content::InputReply {
                value: value.into(),
                extra: Default::default(),
            },
        );
        m.parent_header = Some(input_request.header.clone());
        self.send(Channel::Stdin, m).await
    }

    /// Interrupt per the kernelspec's `interrupt_mode`.
    pub async fn interrupt(&self) -> Result<()> {
        match (self.inner.interrupt_mode, &self.inner.process) {
            (InterruptMode::Signal, Some(p)) => match p.interrupt().await {
                Err(Error::Unsupported(_)) => self.interrupt_by_message().await,
                other => other,
            },
            _ => self.interrupt_by_message().await,
        }
    }

    async fn interrupt_by_message(&self) -> Result<()> {
        let m = self.message("interrupt_request", &content::InterruptRequest::default());
        self.request(Channel::Control, m, false, Duration::from_secs(5))
            .await
            .map(|_| ())
    }

    /// Ask the kernel to exit; escalate to a kill if it does not.
    pub async fn shutdown(&self, restart: bool, grace: Duration) -> Result<()> {
        if self.is_alive() {
            let m = self.message(
                "shutdown_request",
                &content::ShutdownRequest {
                    restart,
                    extra: Default::default(),
                },
            );
            let _ = self.request(Channel::Control, m, false, grace).await;
        }
        // Only a process we own can be waited on or killed. An attached
        // kernel gets the request and nothing more.
        if let Some(p) = self.inner.process.as_ref().filter(|p| p.pid().is_some()) {
            if tokio::time::timeout(grace, p.wait()).await.is_err() {
                tracing::warn!("kernel ignored shutdown_request; killing");
                match p.kill().await {
                    Ok(()) | Err(Error::Unsupported(_)) => {}
                    Err(e) => return Err(e),
                }
                let _ = tokio::time::timeout(grace, p.wait()).await;
            }
        }
        self.inner.mark_dead();
        self.inner.sink.close().await;
        Ok(())
    }

    /// Heartbeat through the transport.
    pub async fn heartbeat(&self, timeout: Duration) -> Result<bool> {
        self.inner.sink.heartbeat(timeout).await
    }

    fn ensure_alive(&self) -> Result<()> {
        if self.is_alive() {
            Ok(())
        } else {
            Err(Error::KernelDied)
        }
    }
}

impl Inner {
    fn mark_dead(&self) {
        if self.alive.swap(false, Ordering::SeqCst) {
            // Dropping the senders fails every waiting request.
            self.pending.lock().unwrap().clear();
        }
    }
}

async fn route(inner: Arc<Inner>, mut incoming: crate::transport::Incoming) {
    while let Some((channel, msg)) = incoming.recv().await {
        if channel == Channel::IoPub {
            inner.iopub_seen.store(true, Ordering::SeqCst);
            if msg.msg_type() == "status" {
                if let Ok(s) = msg.content_as::<content::Status>() {
                    inner.state.send_replace(s.execution_state);
                }
            }
        }
        let parent = msg.parent_id().map(str::to_string);
        if let Some(pid) = parent {
            let finished = {
                let mut pending = inner.pending.lock().unwrap();
                let mut finished = None;
                if let Some(p) = pending.get_mut(&pid) {
                    match channel {
                        Channel::IoPub => {
                            if msg.msg_type() == "status" {
                                if msg.content.get("execution_state").and_then(|v| v.as_str())
                                    == Some("idle")
                                {
                                    p.idle_seen = true;
                                }
                            } else {
                                p.outputs.push(msg.clone());
                            }
                        }
                        Channel::Shell | Channel::Control => p.reply = Some(msg.clone()),
                        Channel::Stdin => {}
                    }
                    if p.complete() {
                        finished = Some(pid.clone());
                    }
                }
                if let Some(id) = &finished {
                    let p = pending.remove(id).unwrap();
                    let exec = Execution {
                        reply: p.reply.unwrap(),
                        outputs: p.outputs,
                    };
                    if let Some(tx) = p.done {
                        let _ = tx.send(exec);
                    }
                }
                finished
            };
            let _ = finished;
        }
        let _ = inner.clients.send((channel, msg));
    }
    inner.mark_dead();
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeKernel, ReplyOrder};
    use crate::transport::in_process_pair;

    async fn boot(order: ReplyOrder) -> (KernelSession, content::KernelInfoReply) {
        let (host, kernel) = in_process_pair(64);
        FakeKernel::spawn(kernel, order);
        let s = KernelSession::start(host, None);
        let info = s.nudge(Duration::from_secs(5)).await.unwrap();
        (s, info)
    }

    #[tokio::test]
    async fn nudge_returns_kernel_info_with_passthrough_fields() {
        let (s, info) = boot(ReplyOrder::ReplyThenIdle).await;
        assert_eq!(info.implementation, "fake");
        assert_eq!(
            info.extra["capabilities"]["jit"],
            serde_json::Value::Bool(false)
        );
        assert!(s.kernel_info().is_some());
        assert_eq!(s.execution_state(), ExecutionState::Idle);
    }

    #[tokio::test]
    async fn execute_completes_in_either_order_and_collects_outputs() {
        for order in [ReplyOrder::ReplyThenIdle, ReplyOrder::IdleThenReply] {
            let (s, _) = boot(order).await;
            let e = s
                .execute(
                    content::ExecuteRequest::new("hello"),
                    Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(e.reply_status(), Some(content::ReplyStatus::Ok));
            let types: Vec<_> = e.outputs.iter().map(|m| m.msg_type().to_string()).collect();
            assert_eq!(types, vec!["execute_input", "stream", "execute_result"]);
            let stream: content::Stream = e.outputs[1].content_as().unwrap();
            assert_eq!(stream.text, "echo: hello\n");
            assert_eq!(s.execution_state(), ExecutionState::Idle);
        }
    }

    #[tokio::test]
    async fn error_reply_and_subscribers_see_everything() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let mut sub = s.subscribe();
        let e = s
            .execute(
                content::ExecuteRequest::new("raise"),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(e.reply_status(), Some(content::ReplyStatus::Error));
        assert_eq!(e.outputs.last().unwrap().msg_type(), "error");
        let mut seen = Vec::new();
        while let Ok((ch, m)) = sub.try_recv() {
            seen.push((ch, m.msg_type().to_string()));
        }
        assert!(seen.contains(&(Channel::IoPub, "error".into())));
        assert!(seen.contains(&(Channel::Shell, "execute_reply".into())));
    }

    #[tokio::test]
    async fn stdin_round_trip() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let mut sub = s.subscribe();
        let s2 = s.clone();
        let answer = tokio::spawn(async move {
            loop {
                let (ch, m) = sub.recv().await.unwrap();
                if ch == Channel::Stdin && m.msg_type() == "input_request" {
                    s2.reply_input(&m, "forty-two").await.unwrap();
                    return;
                }
            }
        });
        let e = s
            .execute(
                content::ExecuteRequest::new("input"),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        answer.await.unwrap();
        let stream: content::Stream = e
            .outputs
            .iter()
            .find(|m| m.msg_type() == "stream")
            .unwrap()
            .content_as()
            .unwrap();
        assert_eq!(stream.text, "you said: forty-two\n");
    }

    #[tokio::test]
    async fn interrupt_by_message_aborts_a_busy_cell() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let s2 = s.clone();
        let run = tokio::spawn(async move {
            s2.execute(
                content::ExecuteRequest::new("sleep 10000"),
                Duration::from_secs(5),
            )
            .await
        });
        let mut st = s.watch_state();
        while *st.borrow() != ExecutionState::Busy {
            st.changed().await.unwrap();
        }
        s.interrupt().await.unwrap();
        let e = run.await.unwrap().unwrap();
        assert_eq!(e.reply_status(), Some(content::ReplyStatus::Error));
    }

    #[tokio::test]
    async fn shutdown_marks_dead_and_requests_fail_after() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        s.shutdown(false, Duration::from_secs(2)).await.unwrap();
        assert!(!s.is_alive());
        assert!(matches!(
            s.execute(content::ExecuteRequest::new("x"), Duration::from_secs(1))
                .await,
            Err(Error::KernelDied)
        ));
    }

    #[tokio::test]
    async fn kernel_that_vanishes_fails_pending_requests() {
        let (host, kernel) = in_process_pair(8);
        let s = KernelSession::start(host, None);
        let s2 = s.clone();
        let pending = tokio::spawn(async move {
            s2.request(
                Channel::Shell,
                s2.message("kernel_info_request", &serde_json::json!({})),
                false,
                Duration::from_secs(5),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(kernel);
        assert!(matches!(pending.await.unwrap(), Err(Error::KernelDied)));
    }
}
