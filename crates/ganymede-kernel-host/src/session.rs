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
//! - Death: the process exiting, the transport closing, or, for a kernel
//!   with no process handle, missed heartbeats.

use crate::process::{KernelProcess, LaunchedKernel};
use crate::transport::{KernelTransport, TransportSink};
use crate::{Error, InterruptMode, Result};
use ganymede_protocol::content::{self, ExecutionState};
use ganymede_protocol::{Channel, Message};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, watch};

/// One request's progress until it is complete.
struct Pending {
    want_idle: bool,
    reply: Option<Message>,
    idle_seen: bool,
    outputs: Vec<Message>,
    status: Vec<Message>,
    done: Option<oneshot::Sender<Execution>>,
}

impl Pending {
    fn complete(&self) -> bool {
        self.reply.is_some() && (self.idle_seen || !self.want_idle)
    }
}

/// A finished request: its reply and the iopub messages it produced.
#[derive(Debug, Clone)]
pub struct Execution {
    pub reply: Message,
    /// Every iopub message for the request except `status`, in arrival order.
    pub outputs: Vec<Message>,
    /// The request's iopub `status` messages (`busy`, then `idle`). Their
    /// header dates are the execution timings `nbclient` records.
    pub status: Vec<Message>,
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
    /// Goes false once, when the kernel dies or is shut down.
    alive: watch::Sender<bool>,
    session_id: String,
    username: String,
    process: Option<Arc<dyn KernelProcess>>,
    interrupt_mode: InterruptMode,
    kernel_info: Mutex<Option<content::KernelInfoReply>>,
    kernel_info_json: Mutex<Option<serde_json::Value>>,
}

/// How a session checks on a kernel it has no process handle for, such as
/// an attached one: a heartbeat every `period`, and after `misses` in a row
/// the kernel is dead. Misses count only after the first answer, so a
/// kernel that is still starting, or has no heartbeat, is never declared
/// dead by this.
#[derive(Debug, Clone, Copy)]
pub struct Heartbeat {
    pub period: Duration,
    pub misses: u32,
}

impl Default for Heartbeat {
    /// jupyter_client's restarter polls every 3 s.
    fn default() -> Self {
        Heartbeat {
            period: Duration::from_secs(3),
            misses: 3,
        }
    }
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
        Self::start_with(transport, launched, Some(Heartbeat::default()))
    }

    /// [`KernelSession::start`] with a chosen heartbeat, or none. The
    /// heartbeat runs only when there is no process to wait on.
    pub fn start_with(
        transport: KernelTransport,
        launched: Option<&LaunchedKernel>,
        heartbeat: Option<Heartbeat>,
    ) -> Self {
        let (clients, _) = broadcast::channel(1024);
        let (state, _) = watch::channel(ExecutionState::Starting);
        let inner = Arc::new(Inner {
            sink: transport.sink,
            clients,
            pending: Mutex::new(HashMap::new()),
            state,
            iopub_seen: AtomicBool::new(false),
            alive: watch::channel(true).0,
            session_id: uuid::Uuid::new_v4().to_string(),
            username: whoami(),
            process: launched.map(|l| l.process.clone()),
            interrupt_mode: launched
                .map(|l| l.interrupt_mode)
                .unwrap_or(InterruptMode::Message),
            kernel_info: Mutex::new(None),
            kernel_info_json: Mutex::new(None),
        });
        tokio::spawn(route(inner.clone(), transport.incoming));
        if let Some(p) = inner.process.clone() {
            let weak = Arc::downgrade(&inner);
            let mut alive = inner.alive.subscribe();
            tokio::spawn(async move {
                let exited = tokio::select! {
                    _ = p.wait() => true,
                    _ = alive.wait_for(|a| !*a) => false,
                };
                if let Some(inner) = weak.upgrade().filter(|_| exited) {
                    inner.linger_for_last_replies().await;
                    inner.mark_dead();
                }
            });
        }
        // A process handle with no pid (an attached kernel) never exits,
        // so only the heartbeat can tell that the kernel is gone.
        let owns_process = inner.process.as_ref().is_some_and(|p| p.pid().is_some());
        if let (false, Some(hb)) = (owns_process, heartbeat) {
            tokio::spawn(monitor(Arc::downgrade(&inner), hb));
        }
        KernelSession { inner }
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn is_alive(&self) -> bool {
        *self.inner.alive.borrow()
    }

    /// Changes to false once, when the kernel dies or is shut down. The
    /// receiver does not keep the session alive.
    pub fn watch_alive(&self) -> watch::Receiver<bool> {
        self.inner.alive.subscribe()
    }

    /// Resolves when the kernel is dead.
    pub async fn dead(&self) {
        let mut rx = self.watch_alive();
        let _ = rx.wait_for(|alive| !*alive).await;
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

    /// The last `kernel_info_reply` content exactly as the kernel sent it.
    pub fn kernel_info_json(&self) -> Option<serde_json::Value> {
        self.inner.kernel_info_json.lock().unwrap().clone()
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
                status: Vec::new(),
                done: Some(tx),
            },
        );
        // Removes the entry however this future ends: an error, a timeout,
        // or being dropped by a cancelled caller. Otherwise `route` would
        // keep collecting the request's iopub output until the kernel dies.
        let _guard = PendingGuard {
            inner: &self.inner,
            id,
        };
        self.inner.sink.send(channel, msg).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(exec)) => Ok(exec),
            Ok(Err(_)) => Err(Error::KernelDied),
            Err(_) => Err(Error::Timeout("reply")),
        }
    }

    /// Number of requests waiting for a reply.
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.inner.pending.lock().unwrap().len()
    }

    /// The startup handshake. Returns the kernel's info once shell and
    /// control both answer and iopub has delivered at least one message.
    ///
    /// Each unanswered `kernel_info_request` waits longer than the last
    /// (0.5 s, doubling to 4 s) before another is sent. A kernel that
    /// answers on shell only once its runtime is up, on a serial loop
    /// (cdp-kernel), otherwise collects a queue of them during a slow
    /// start and answers each with a busy/idle pair.
    pub async fn nudge(&self, timeout: Duration) -> Result<content::KernelInfoReply> {
        const FIRST: Duration = Duration::from_millis(500);
        const LONGEST: Duration = Duration::from_secs(4);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut shell_info: Option<(content::KernelInfoReply, serde_json::Value)> = None;
        let mut control_ok = false;
        let (mut shell_wait, mut control_wait) = (FIRST, FIRST);
        while tokio::time::Instant::now() < deadline {
            let left = deadline - tokio::time::Instant::now();
            let req = content::KernelInfoRequest::default();
            // Every request makes the kernel publish status on iopub, so
            // keep asking until the subscription has caught one: the SUB
            // socket can connect after shell and control have answered.
            let ask_shell = shell_info.is_none() || !self.inner.iopub_seen.load(Ordering::SeqCst);
            let shell = async {
                if !ask_shell {
                    return None;
                }
                let m = self.message("kernel_info_request", &req);
                Some(
                    self.request(Channel::Shell, m, false, shell_wait.min(left))
                        .await,
                )
            };
            let control = async {
                if control_ok {
                    return None;
                }
                let m = self.message("kernel_info_request", &req);
                Some(
                    self.request(Channel::Control, m, false, control_wait.min(left))
                        .await,
                )
            };
            let (shell, control) = tokio::join!(shell, control);
            match shell {
                Some(Ok(exec)) => {
                    shell_info = exec
                        .reply
                        .content_as()
                        .ok()
                        .map(|info| (info, exec.reply.content.clone()));
                }
                Some(Err(_)) => shell_wait = (shell_wait * 2).min(LONGEST),
                None => {}
            }
            match control {
                Some(Ok(_)) => control_ok = true,
                Some(Err(_)) => control_wait = (control_wait * 2).min(LONGEST),
                None => {}
            }
            if let (Some((info, json)), true, true) = (
                &shell_info,
                control_ok,
                self.inner.iopub_seen.load(Ordering::SeqCst),
            ) {
                *self.inner.kernel_info.lock().unwrap() = Some(info.clone());
                *self.inner.kernel_info_json.lock().unwrap() = Some(json.clone());
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
        self.ensure_alive()?;
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
        // cdp-kernel waits up to 5 s for its target to stop before it
        // replies, so allow more than that.
        self.request(Channel::Control, m, false, Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    /// Ask the kernel to exit; escalate if it does not. As in
    /// `jupyter_client`'s `finish_shutdown`: a `shutdown_request` (its
    /// reply awaited up to `grace`), then up to `grace / 2` for the process
    /// to exit, then SIGTERM and another `grace / 2`, then SIGKILL.
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
            let half = grace / 2;
            let mut exited = tokio::time::timeout(half, p.wait()).await.is_ok();
            if !exited {
                tracing::warn!("kernel ignored shutdown_request; terminating");
                match p.terminate().await {
                    Ok(()) => exited = tokio::time::timeout(half, p.wait()).await.is_ok(),
                    Err(Error::Unsupported(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            if !exited {
                tracing::warn!("kernel still running; killing");
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

struct PendingGuard<'a> {
    inner: &'a Inner,
    id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.inner.pending.lock().unwrap().remove(&self.id);
    }
}

impl Inner {
    /// After the process exits, give requests still waiting up to
    /// [`EXIT_LINGER`] to get their replies: a kernel that answers and
    /// then exits (a `shutdown_request` on shell, a cell that ends the
    /// process) sent them before exiting, but they can still be on their
    /// way through the sockets when the exit is seen. A `jupyter_client`
    /// client reads them off the socket regardless.
    async fn linger_for_last_replies(&self) {
        let deadline = tokio::time::Instant::now() + EXIT_LINGER;
        while !self.pending.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn mark_dead(&self) {
        if self.alive.send_replace(false) {
            // Dropping the senders fails every waiting request.
            self.pending.lock().unwrap().clear();
            // Stop the transport's tasks so `route` sees its channel close
            // and a ZMQ SUB socket stops reconnecting to a dead kernel.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let sink = self.sink.clone();
                rt.spawn(async move { sink.close().await });
            }
        }
    }
}

/// How long requests may still get replies after the kernel exits.
const EXIT_LINGER: Duration = Duration::from_millis(500);

/// Heartbeat a kernel with no process to wait on, and mark it dead after
/// `hb.misses` missed beats in a row.
async fn monitor(inner: Weak<Inner>, hb: Heartbeat) {
    let Some(mut alive) = inner.upgrade().map(|i| i.alive.subscribe()) else {
        return;
    };
    let mut answered = false;
    let mut missed = 0;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(hb.period) => {}
            _ = alive.wait_for(|a| !*a) => return,
        }
        let Some(inner) = inner.upgrade() else { return };
        // Connecting the heartbeat socket can itself hang, so bound the
        // whole beat, not only the wait for the echo.
        let ok = matches!(
            tokio::time::timeout(hb.period, inner.sink.heartbeat(hb.period)).await,
            Ok(Ok(true))
        );
        if ok {
            answered = true;
            missed = 0;
        } else if answered {
            missed += 1;
            if missed >= hb.misses {
                tracing::warn!(session = %inner.session_id, "kernel missed {missed} heartbeats; marking it dead");
                inner.mark_dead();
                return;
            }
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
                                p.status.push(msg.clone());
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
                        status: p.status,
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

    #[tokio::test(start_paused = true)]
    async fn nudge_backs_off_while_shell_is_slow_to_start() {
        let (host, kernel) = in_process_pair(64);
        let (_k, asked) = FakeKernel::spawn_slow_shell(
            kernel,
            ReplyOrder::ReplyThenIdle,
            Duration::from_secs(30),
        );
        let s = KernelSession::start(host, None);
        let info = s.nudge(Duration::from_secs(60)).await.unwrap();
        assert_eq!(info.implementation, "fake");
        // Re-sending every 500 ms queued ~50 requests; backing off to 4 s
        // queues about ten.
        let n = asked.load(std::sync::atomic::Ordering::SeqCst);
        assert!(n <= 11, "{n} kernel_info_requests queued on shell");
        assert_eq!(s.pending_len(), 0);
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

    #[tokio::test]
    async fn kernel_that_dies_mid_cell_is_reported_dead() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let mut alive = s.watch_alive();
        assert!(matches!(
            s.execute(content::ExecuteRequest::new("die"), Duration::from_secs(5))
                .await,
            Err(Error::KernelDied)
        ));
        tokio::time::timeout(Duration::from_secs(1), s.dead())
            .await
            .unwrap();
        alive.wait_for(|a| !*a).await.unwrap();
        assert!(!s.is_alive());
    }

    #[tokio::test]
    async fn exit_mid_cell_after_output_fails_the_request() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let mut sub = s.subscribe();
        let r = s
            .execute(content::ExecuteRequest::new("exit"), Duration::from_secs(5))
            .await;
        assert!(matches!(r, Err(Error::KernelDied)), "{:?}", r.err());
        // What it printed before vanishing still reached subscribers.
        let mut texts = Vec::new();
        while let Ok((_, m)) = sub.try_recv() {
            if m.msg_type() == "stream" {
                texts.push(m.content["text"].clone());
            }
        }
        assert_eq!(texts, ["partial\n"]);
        assert!(!s.is_alive());
    }

    #[tokio::test]
    async fn displays_updates_and_clears_are_collected_in_order() {
        let (s, _) = boot(ReplyOrder::IdleThenReply).await;
        let e = s
            .execute(
                content::ExecuteRequest::new("display"),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        let types: Vec<_> = e.outputs.iter().map(|m| m.msg_type()).collect();
        assert_eq!(
            types,
            [
                "execute_input",
                "display_data",
                "update_display_data",
                "clear_output",
                "stream"
            ]
        );
        assert_eq!(e.outputs[2].content["transient"]["display_id"], "d1");
    }

    #[tokio::test]
    async fn queued_executes_are_aborted_after_an_error() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let req =
            |code: &str| s.execute(content::ExecuteRequest::new(code), Duration::from_secs(5));
        let (a, b, c) = tokio::join!(req("raise"), req("after"), req("later"));
        let status = |e: &Execution| e.reply.content["status"].clone();
        assert_eq!(status(&a.unwrap()), "error");
        assert_eq!(status(&b.unwrap()), "aborted");
        assert_eq!(status(&c.unwrap()), "aborted");
        // Without stop_on_error the next cell runs.
        let mut raise = content::ExecuteRequest::new("raise");
        raise.stop_on_error = false;
        let (a, b) = tokio::join!(s.execute(raise, Duration::from_secs(5)), req("after"));
        assert_eq!(status(&a.unwrap()), "error");
        assert_eq!(status(&b.unwrap()), "ok");
        assert_eq!(s.execution_state(), ExecutionState::Idle);
    }

    #[tokio::test]
    async fn comms_echo_binary_buffers_and_unknown_targets_close() {
        let (s, _) = boot(ReplyOrder::ReplyThenIdle).await;
        let mut sub = s.subscribe();
        let send = |msg_type: &str, content: serde_json::Value| {
            let m = s.message(msg_type, &content);
            s.send(Channel::Shell, m)
        };
        async fn next(
            sub: &mut broadcast::Receiver<(Channel, Message)>,
            msg_type: &str,
        ) -> Message {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let (_, m) = sub.recv().await.unwrap();
                    if m.msg_type() == msg_type {
                        return m;
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("no {msg_type}"))
        }

        send(
            "comm_open",
            serde_json::json!({"comm_id": "nobody", "target_name": "nobody", "data": {}}),
        )
        .await
        .unwrap();
        assert_eq!(
            next(&mut sub, "comm_close").await.content["comm_id"],
            "nobody"
        );

        send(
            "comm_open",
            serde_json::json!({"comm_id": "c1", "target_name": "echo", "data": {}}),
        )
        .await
        .unwrap();
        let bytes: Vec<u8> = (0..=255).collect();
        let m = s
            .message(
                "comm_msg",
                &serde_json::json!({"comm_id": "c1", "data": {"x": 1}}),
            )
            .with_buffers(vec![bytes.clone().into(), Vec::new().into()]);
        s.send(Channel::Shell, m).await.unwrap();
        let echo = next(&mut sub, "comm_msg").await;
        assert_eq!(echo.content["data"], serde_json::json!({"x": 1}));
        assert_eq!(echo.buffers.len(), 2);
        assert_eq!(echo.buffers[0].as_ref(), &bytes[..]);
        assert!(echo.buffers[1].is_empty());

        let info = |target: Option<&str>| {
            let c = match target {
                Some(t) => serde_json::json!({ "target_name": t }),
                None => serde_json::json!({}),
            };
            let m = s.message("comm_info_request", &c);
            s.request(Channel::Shell, m, false, Duration::from_secs(5))
        };
        let comms = info(None).await.unwrap().reply.content["comms"].clone();
        assert_eq!(comms, serde_json::json!({"c1": {"target_name": "echo"}}));
        let comms = info(Some("other")).await.unwrap().reply.content["comms"].clone();
        assert_eq!(comms, serde_json::json!({}));
        send(
            "comm_close",
            serde_json::json!({"comm_id": "c1", "data": {}}),
        )
        .await
        .unwrap();
        let comms = info(None).await.unwrap().reply.content["comms"].clone();
        assert_eq!(comms, serde_json::json!({}));

        // A kernel-side open reaches subscribers.
        s.execute(content::ExecuteRequest::new("comm"), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            next(&mut sub, "comm_open").await.content["target_name"],
            "fake.target"
        );
    }

    /// A transport whose kernel answers heartbeats while `beating` is true
    /// and never closes its incoming channel, like a ZMQ SUB socket waiting
    /// on a dead kernel.
    struct SilentSink {
        beating: Arc<AtomicBool>,
        closed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TransportSink for SilentSink {
        async fn send(&self, _channel: Channel, _msg: Message) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _timeout: Duration) -> Result<bool> {
            Ok(self.beating.load(Ordering::SeqCst))
        }
        async fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn silent_session(
        beating: bool,
        hb: Heartbeat,
    ) -> (KernelSession, Arc<AtomicBool>, Arc<AtomicBool>) {
        let beating = Arc::new(AtomicBool::new(beating));
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, incoming) = tokio::sync::mpsc::channel(1);
        // Hold the sender so `route` never ends on its own.
        tokio::spawn(async move { tx.closed().await });
        let transport = KernelTransport {
            sink: Arc::new(SilentSink {
                beating: beating.clone(),
                closed: closed.clone(),
            }),
            incoming,
        };
        let launched = crate::attach(ganymede_protocol::ConnectionInfo::new_tcp(
            "127.0.0.1",
            [1, 2, 3, 4, 5],
        ));
        let s = KernelSession::start_with(transport, Some(&launched), Some(hb));
        (s, beating, closed)
    }

    const FAST: Heartbeat = Heartbeat {
        period: Duration::from_millis(20),
        misses: 3,
    };

    #[tokio::test]
    async fn cancelled_and_timed_out_requests_leave_no_pending_entry() {
        let (s, _, _) = silent_session(true, FAST);
        let s2 = s.clone();
        let task = tokio::spawn(async move {
            let m = s2.message("kernel_info_request", &serde_json::json!({}));
            s2.request(Channel::Shell, m, false, Duration::from_secs(60))
                .await
        });
        while s.pending_len() == 0 {
            tokio::task::yield_now().await;
        }
        task.abort();
        let _ = task.await;
        assert_eq!(s.pending_len(), 0);

        let m = s.message("kernel_info_request", &serde_json::json!({}));
        let r = s
            .request(Channel::Shell, m, false, Duration::from_millis(10))
            .await;
        assert!(matches!(r, Err(Error::Timeout(_))));
        assert_eq!(s.pending_len(), 0);
    }

    /// A process that records signals and exits on the first one listed in
    /// `exits_on`.
    struct FakeProcess {
        calls: Mutex<Vec<&'static str>>,
        exits_on: &'static str,
        exit: watch::Sender<bool>,
    }

    impl FakeProcess {
        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
            if call == self.exits_on {
                self.exit.send_replace(true);
            }
        }
    }

    #[async_trait::async_trait]
    impl KernelProcess for FakeProcess {
        fn pid(&self) -> Option<u32> {
            Some(1)
        }
        async fn interrupt(&self) -> Result<()> {
            self.record("interrupt");
            Ok(())
        }
        async fn terminate(&self) -> Result<()> {
            self.record("terminate");
            Ok(())
        }
        async fn kill(&self) -> Result<()> {
            self.record("kill");
            Ok(())
        }
        async fn wait(&self) -> Option<i32> {
            let _ = self.exit.subscribe().wait_for(|e| *e).await;
            Some(0)
        }
        fn is_alive(&self) -> bool {
            !*self.exit.borrow()
        }
    }

    /// A session over a silent transport (no replies) with a fake process.
    fn session_with_process(exits_on: &'static str) -> (KernelSession, Arc<FakeProcess>) {
        let (tx, incoming) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move { tx.closed().await });
        let transport = KernelTransport {
            sink: Arc::new(SilentSink {
                beating: Arc::new(AtomicBool::new(true)),
                closed: Arc::new(AtomicBool::new(false)),
            }),
            incoming,
        };
        let process = Arc::new(FakeProcess {
            calls: Mutex::new(Vec::new()),
            exits_on,
            exit: watch::channel(false).0,
        });
        let launched = LaunchedKernel {
            connection: ganymede_protocol::ConnectionInfo::new_tcp("127.0.0.1", [1, 2, 3, 4, 5]),
            connection_file: None,
            process: process.clone(),
            interrupt_mode: InterruptMode::Signal,
        };
        (KernelSession::start(transport, Some(&launched)), process)
    }

    #[tokio::test]
    async fn a_reply_sent_just_before_exit_still_arrives() {
        let (host, mut kernel) = in_process_pair(8);
        let process = Arc::new(FakeProcess {
            calls: Mutex::new(Vec::new()),
            exits_on: "",
            exit: watch::channel(false).0,
        });
        let launched = LaunchedKernel {
            connection: ganymede_protocol::ConnectionInfo::new_tcp("127.0.0.1", [1, 2, 3, 4, 5]),
            connection_file: None,
            process: process.clone(),
            interrupt_mode: InterruptMode::Signal,
        };
        let s = KernelSession::start(host, Some(&launched));
        let p = process.clone();
        tokio::spawn(async move {
            let (ch, req) = kernel.incoming.recv().await.unwrap();
            // The process is seen to exit before its reply comes through.
            p.exit.send_replace(true);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let reply =
                Message::reply_to(&req, "shutdown_reply", &serde_json::json!({"status": "ok"}));
            let _ = kernel.outgoing.send((ch, reply)).await;
            // Keep the kernel side open.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let m = s.message("shutdown_request", &serde_json::json!({"restart": false}));
        let exec = s
            .request(Channel::Shell, m, false, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(exec.reply.msg_type(), "shutdown_reply");
        tokio::time::timeout(Duration::from_secs(2), s.dead())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_sends_sigterm_before_sigkill() {
        let grace = Duration::from_millis(100);
        let (s, p) = session_with_process("terminate");
        s.shutdown(false, grace).await.unwrap();
        assert_eq!(*p.calls.lock().unwrap(), vec!["terminate"]);

        let (s, p) = session_with_process("kill");
        s.shutdown(false, grace).await.unwrap();
        assert_eq!(*p.calls.lock().unwrap(), vec!["terminate", "kill"]);

        // A dead kernel is not signalled.
        assert!(matches!(s.interrupt().await, Err(Error::KernelDied)));
        assert_eq!(p.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn missed_heartbeats_mark_an_attached_kernel_dead() {
        let (s, beating, closed) = silent_session(true, FAST);
        tokio::time::sleep(FAST.period * 3).await;
        assert!(s.is_alive());
        beating.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(2), s.dead())
            .await
            .unwrap();
        // The transport is closed so its tasks stop.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_kernel_that_never_answered_a_heartbeat_is_not_declared_dead() {
        let (s, _, _) = silent_session(false, FAST);
        tokio::time::sleep(FAST.period * 10).await;
        assert!(s.is_alive());
    }
}
