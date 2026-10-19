//! One running kernel, and what a host does with it.

use crate::{Error, Result};
use ganymede_kernel_host::protocol::content::{self, ExecuteRequest, KernelInfoReply, ReplyStatus};
use ganymede_kernel_host::protocol::{Channel, ConnectionInfo, Message};
use ganymede_kernel_host::{
    KernelSession, Kernelspec, LaunchOptions, LaunchedKernel, Provisioner, TransportFactory,
};
use ganymede_nbformat::{Notebook, Output};
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Requests that are not executions get this long to answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// "No timeout", without overflowing an `Instant`.
const FOREVER: Duration = Duration::from_secs(10 * 365 * 24 * 3600);
/// Launches tried before a kernel that never answers counts as failed.
const START_ATTEMPTS: u32 = 3;

/// What it takes to launch the same kernel again.
pub(crate) struct Relaunch {
    pub provisioner: Arc<dyn Provisioner>,
    pub transports: Arc<dyn TransportFactory>,
    pub spec: Kernelspec,
    pub opts: LaunchOptions,
}

impl Relaunch {
    pub async fn launch(&self) -> Result<(KernelSession, LaunchedKernel)> {
        let launched = self
            .provisioner
            .launch(&self.spec, self.opts.clone())
            .await?;
        let transport = match self.transports.connect(&launched).await {
            Ok(t) => t,
            Err(e) => {
                let _ = launched.process.kill().await;
                return Err(e.into());
            }
        };
        let session = KernelSession::start(transport, Some(&launched));
        Ok((session, launched))
    }

    /// Launch and wait for `kernel_info_reply`. Ports are free when picked
    /// but belong to whichever process binds them first, so two kernels
    /// launched at once can be given the same one; the loser exits or never
    /// answers. Such a launch is retried on fresh ports.
    pub async fn start(
        &self,
        timeout: Duration,
    ) -> Result<(KernelSession, LaunchedKernel, KernelInfoReply)> {
        use ganymede_kernel_host::Error as HostError;
        let mut attempt = 1;
        loop {
            let (session, launched) = self.launch().await?;
            match session.nudge(timeout).await {
                Ok(info) => return Ok((session, launched, info)),
                Err(e) => {
                    let _ = session.shutdown(false, Duration::from_millis(200)).await;
                    let retry = matches!(e, HostError::KernelDied | HostError::Timeout(_));
                    if !retry || attempt == START_ATTEMPTS {
                        return Err(e.into());
                    }
                    tracing::warn!(kernel = %self.spec.name, "kernel failed to start ({e}); launching again");
                    attempt += 1;
                }
            }
        }
    }
}

struct Current {
    session: KernelSession,
    launched: Option<LaunchedKernel>,
    info: KernelInfoReply,
    info_json: Value,
}

/// A running kernel. `Send + Sync`: share it in an `Arc` to interrupt
/// from one task while another executes.
pub struct Kernel {
    current: RwLock<Current>,
    spec: Option<Kernelspec>,
    relaunch: Option<Relaunch>,
    startup_timeout: Duration,
}

/// Per-execution settings. The defaults are a notebook cell's.
#[derive(Debug, Clone)]
pub struct ExecuteOptions {
    pub silent: bool,
    pub store_history: bool,
    /// Let the kernel ask for input. [`Handler::on_input`] answers.
    pub allow_stdin: bool,
    /// Abort queued executions after an error.
    pub stop_on_error: bool,
    /// `None` waits until the kernel replies.
    pub timeout: Option<Duration>,
}

impl Default for ExecuteOptions {
    fn default() -> Self {
        ExecuteOptions {
            silent: false,
            store_history: true,
            allow_stdin: false,
            stop_on_error: true,
            timeout: None,
        }
    }
}

/// Receives an execution's messages as they arrive.
#[async_trait::async_trait]
pub trait Handler: Send {
    /// Every iopub message for the request except `status`: streams,
    /// displays, results, errors, clears, comms.
    fn on_iopub(&mut self, _msg: &Message) {}
    /// The kernel asked for input. `None` answers with an empty string.
    /// Async, so a UI can wait for the user without blocking the runtime.
    async fn on_input(&mut self, _prompt: &str, _password: bool) -> Option<String> {
        None
    }
}

/// A handler that ignores everything.
pub struct NoHandler;
#[async_trait::async_trait]
impl Handler for NoHandler {}

/// A finished execution.
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    pub status: ReplyStatus,
    pub execution_count: Option<u64>,
    /// The cell's outputs, built as a notebook would show them.
    pub outputs: Vec<Output>,
    /// `execute_reply` content.
    pub reply: Value,
    /// Every iopub message for the request except `status`.
    pub messages: Vec<Message>,
}

impl ExecuteResult {
    pub fn is_ok(&self) -> bool {
        self.status == ReplyStatus::Ok
    }

    /// `(ename, evalue)` from an error reply.
    pub fn error(&self) -> Option<(&str, &str)> {
        (self.status == ReplyStatus::Error).then(|| {
            (
                self.reply["ename"].as_str().unwrap_or_default(),
                self.reply["evalue"].as_str().unwrap_or_default(),
            )
        })
    }

    /// Streams and the `text/plain` of results and displays, joined: what
    /// a terminal would print.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for o in &self.outputs {
            match o {
                Output::Stream(st) => s.push_str(st.text.as_str()),
                Output::ExecuteResult(r) => {
                    push_line(&mut s, r.data.get_str("text/plain").unwrap_or_default())
                }
                Output::DisplayData(d) => {
                    push_line(&mut s, d.data.get_str("text/plain").unwrap_or_default())
                }
                Output::Error(e) => push_line(&mut s, &format!("{}: {}", e.ename, e.evalue)),
                Output::Unknown(_) => {}
            }
        }
        s
    }
}

fn push_line(s: &mut String, line: &str) {
    s.push_str(line);
    if !line.ends_with('\n') {
        s.push('\n');
    }
}

/// `is_complete_reply`, for a console deciding whether to run or keep
/// reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsComplete {
    Complete,
    Incomplete { indent: String },
    Invalid,
    Unknown,
}

impl Kernel {
    pub(crate) async fn ready(
        session: KernelSession,
        launched: Option<LaunchedKernel>,
        spec: Option<Kernelspec>,
        relaunch: Option<Relaunch>,
        startup_timeout: Duration,
    ) -> Result<Kernel> {
        let info = session.nudge(startup_timeout).await?;
        Ok(Kernel::started(
            session,
            launched,
            info,
            spec,
            relaunch,
            startup_timeout,
        ))
    }

    pub(crate) fn started(
        session: KernelSession,
        launched: Option<LaunchedKernel>,
        info: KernelInfoReply,
        spec: Option<Kernelspec>,
        relaunch: Option<Relaunch>,
        startup_timeout: Duration,
    ) -> Kernel {
        let info_json = session.kernel_info_json().unwrap_or(Value::Null);
        Kernel {
            current: RwLock::new(Current {
                session,
                launched,
                info,
                info_json,
            }),
            spec,
            relaunch,
            startup_timeout,
        }
    }

    /// The protocol session underneath, for anything this type does not
    /// wrap. Replaced on restart.
    pub fn session(&self) -> KernelSession {
        self.current.read().unwrap().session.clone()
    }

    pub fn info(&self) -> KernelInfoReply {
        self.current.read().unwrap().info.clone()
    }

    /// `kernel_info_reply` content as the kernel sent it, extra fields
    /// included.
    pub fn info_json(&self) -> Value {
        self.current.read().unwrap().info_json.clone()
    }

    /// The kernelspec, when the engine launched this kernel.
    pub fn spec(&self) -> Option<&Kernelspec> {
        self.spec.as_ref()
    }

    pub fn connection_info(&self) -> Option<ConnectionInfo> {
        let c = self.current.read().unwrap();
        c.launched.as_ref().and_then(|l| l.connection.clone())
    }

    pub fn connection_file(&self) -> Option<std::path::PathBuf> {
        let c = self.current.read().unwrap();
        c.launched.as_ref().and_then(|l| l.connection_file.clone())
    }

    pub fn is_alive(&self) -> bool {
        self.session().is_alive()
    }

    /// True when the engine launched the process and can restart it.
    pub fn owns_process(&self) -> bool {
        self.relaunch.is_some()
    }

    /// Execute with the defaults and no streaming.
    pub async fn execute(&self, code: &str) -> Result<ExecuteResult> {
        self.execute_with(code, &ExecuteOptions::default(), &mut NoHandler)
            .await
    }

    /// Execute, handing each message to `handler` as it arrives and
    /// answering input requests through it.
    pub async fn execute_with(
        &self,
        code: &str,
        opts: &ExecuteOptions,
        handler: &mut dyn Handler,
    ) -> Result<ExecuteResult> {
        let session = self.session();
        let mut req = ExecuteRequest::new(code);
        req.silent = opts.silent;
        req.store_history = opts.store_history;
        req.allow_stdin = opts.allow_stdin;
        req.stop_on_error = opts.stop_on_error;
        let msg = session.message("execute_request", &req);
        let id = msg.header.msg_id.clone();
        // Subscribe before sending so nothing is missed.
        let mut sub = session.subscribe();
        let request = session.request(Channel::Shell, msg, true, opts.timeout.unwrap_or(FOREVER));
        tokio::pin!(request);
        // By msg_id, not by count: after a lag the broadcast skips some
        // messages, and the reply's record fills in exactly those.
        let mut streamed = std::collections::HashSet::new();
        let exec = loop {
            tokio::select! {
                biased;
                r = sub.recv() => match r {
                    Ok((ch, m)) if m.parent_id() == Some(id.as_str()) => match ch {
                        Channel::IoPub if m.msg_type() != "status" => {
                            handler.on_iopub(&m);
                            streamed.insert(m.header.msg_id.clone());
                        }
                        Channel::Stdin if m.msg_type() == "input_request" => {
                            let prompt = m.content["prompt"].as_str().unwrap_or_default();
                            let password = m.content["password"].as_bool().unwrap_or(false);
                            let value = handler.on_input(prompt, password).await.unwrap_or_default();
                            session.reply_input(&m, &value).await?;
                        }
                        _ => {}
                    },
                    Ok(_) | Err(RecvError::Closed) => {}
                    Err(RecvError::Lagged(n)) => tracing::warn!(
                        "output handler fell {n} messages behind; the rest arrives with the reply"
                    ),
                },
                r = &mut request => match r {
                    // Stop the cell rather than leave the kernel busy; on a
                    // shared kernel the next client would wait behind it.
                    Err(ganymede_kernel_host::Error::Timeout(t)) => {
                        if let Err(e) = session.interrupt().await {
                            tracing::warn!("interrupting a timed-out cell: {e}");
                        }
                        return Err(ganymede_kernel_host::Error::Timeout(t).into());
                    }
                    r => break r?,
                },
            }
        };
        // The reply can win the race against the last broadcasts; the
        // execution's own record has everything, in order.
        for m in exec.outputs.iter() {
            if !streamed.contains(&m.header.msg_id) {
                handler.on_iopub(m);
            }
        }
        let reply: content::ExecuteReply = exec.reply.content_as()?;
        Ok(ExecuteResult {
            status: reply.status,
            execution_count: reply.execution_count,
            outputs: ganymede_exec::outputs_from_messages(&exec.outputs),
            reply: exec.reply.content,
            messages: exec.outputs,
        })
    }

    /// `complete_request`. Returns the reply content (`matches`,
    /// `cursor_start`, `cursor_end`, `metadata`).
    pub async fn complete(&self, code: &str, cursor_pos: usize) -> Result<Value> {
        let req = content::CompleteRequest {
            code: code.into(),
            cursor_pos,
            extra: Default::default(),
        };
        self.shell("complete_request", &req).await
    }

    /// `inspect_request`. Returns the reply content (`found`, `data`).
    pub async fn inspect(&self, code: &str, cursor_pos: usize, detail_level: u8) -> Result<Value> {
        let req = content::InspectRequest {
            code: code.into(),
            cursor_pos,
            detail_level,
            extra: Default::default(),
        };
        self.shell("inspect_request", &req).await
    }

    pub async fn is_complete(&self, code: &str) -> Result<IsComplete> {
        let req = content::IsCompleteRequest {
            code: code.into(),
            extra: Default::default(),
        };
        let r = self.shell("is_complete_request", &req).await?;
        Ok(match r["status"].as_str() {
            Some("complete") => IsComplete::Complete,
            Some("incomplete") => IsComplete::Incomplete {
                indent: r["indent"].as_str().unwrap_or_default().to_string(),
            },
            Some("invalid") => IsComplete::Invalid,
            _ => IsComplete::Unknown,
        })
    }

    /// `comm_info_request`, optionally for one target. Returns `comms`.
    pub async fn comm_info(&self, target_name: Option<&str>) -> Result<Value> {
        let req = match target_name {
            Some(t) => serde_json::json!({ "target_name": t }),
            None => serde_json::json!({}),
        };
        Ok(self.shell("comm_info_request", &req).await?["comms"].take())
    }

    /// Open a comm from the frontend side.
    pub async fn comm_open(&self, comm_id: &str, target_name: &str, data: Value) -> Result<()> {
        let s = self.session();
        let m = s.message(
            "comm_open",
            &serde_json::json!({"comm_id": comm_id, "target_name": target_name, "data": data}),
        );
        Ok(s.send(Channel::Shell, m).await?)
    }

    pub async fn comm_msg(&self, comm_id: &str, data: Value) -> Result<()> {
        let s = self.session();
        let m = s.message(
            "comm_msg",
            &serde_json::json!({"comm_id": comm_id, "data": data}),
        );
        Ok(s.send(Channel::Shell, m).await?)
    }

    pub async fn comm_close(&self, comm_id: &str, data: Value) -> Result<()> {
        let s = self.session();
        let m = s.message(
            "comm_close",
            &serde_json::json!({"comm_id": comm_id, "data": data}),
        );
        Ok(s.send(Channel::Shell, m).await?)
    }

    /// A shell request that expects only a reply. Returns its content.
    pub async fn shell<C: serde::Serialize>(&self, msg_type: &str, content: &C) -> Result<Value> {
        let s = self.session();
        let m = s.message(msg_type, content);
        let exec = s.request(Channel::Shell, m, false, REQUEST_TIMEOUT).await?;
        Ok(exec.reply.content)
    }

    /// Interrupt per the kernelspec: a signal, or `interrupt_request`.
    pub async fn interrupt(&self) -> Result<()> {
        Ok(self.session().interrupt().await?)
    }

    /// Ask the kernel to exit and relaunch it from its kernelspec. State is
    /// lost; the working directory and environment are kept. Only for
    /// kernels the engine launched.
    pub async fn restart(&self) -> Result<()> {
        let relaunch = self
            .relaunch
            .as_ref()
            .ok_or_else(|| Error::Usage("restart needs a kernel this engine launched".into()))?;
        let old = self.session();
        let _ = old.shutdown(true, SHUTDOWN_GRACE).await;
        let (session, launched, info) = relaunch.start(self.startup_timeout).await?;
        let info_json = session.kernel_info_json().unwrap_or(Value::Null);
        *self.current.write().unwrap() = Current {
            session,
            launched: Some(launched),
            info,
            info_json,
        };
        Ok(())
    }

    /// Ask the kernel to exit; kill it if it does not and the engine owns
    /// it. Idempotent.
    pub async fn shutdown(&self) -> Result<()> {
        let s = self.session();
        if s.is_alive() {
            s.shutdown(false, SHUTDOWN_GRACE).await?;
        }
        self.current.write().unwrap().launched.take();
        Ok(())
    }

    /// Run a notebook's code cells with `nbclient`'s semantics, writing
    /// outputs into `nb`.
    pub async fn run_notebook(
        &self,
        nb: &mut Notebook,
        opts: &ganymede_exec::Options,
    ) -> std::result::Result<ganymede_exec::Report, ganymede_exec::Error> {
        ganymede_exec::execute_notebook(&self.session(), nb, opts).await
    }

    /// [`Kernel::run_notebook`], with progress events.
    pub async fn run_notebook_with(
        &self,
        nb: &mut Notebook,
        opts: &ganymede_exec::Options,
        on_event: &mut dyn FnMut(ganymede_exec::Event<'_>),
    ) -> std::result::Result<ganymede_exec::Report, ganymede_exec::Error> {
        ganymede_exec::execute_notebook_with(&self.session(), nb, opts, on_event).await
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        // A kernel the engine launched must not outlive it. `shutdown` is
        // the polite path; this is the backstop.
        if self.relaunch.is_none() {
            return;
        }
        if let Ok(c) = self.current.get_mut() {
            if let Some(l) = &c.launched {
                if l.process.is_alive() {
                    l.process.kill_now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use ganymede_kernel_host::in_process_pair;
    use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};

    async fn fake() -> Kernel {
        let (host, side) = in_process_pair(64);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Engine::new().adopt(host).await.unwrap()
    }

    struct Collect(Vec<String>, &'static str);
    #[async_trait::async_trait]
    impl Handler for Collect {
        fn on_iopub(&mut self, m: &Message) {
            self.0.push(m.msg_type().to_string());
        }
        async fn on_input(&mut self, prompt: &str, _: bool) -> Option<String> {
            self.0.push(format!("input:{prompt}"));
            Some(self.1.into())
        }
    }

    #[tokio::test]
    async fn execute_streams_and_builds_outputs() {
        let k = fake().await;
        assert_eq!(k.info().implementation, "fake");
        let mut h = Collect(vec![], "");
        let r = k
            .execute_with("hello", &ExecuteOptions::default(), &mut h)
            .await
            .unwrap();
        assert!(r.is_ok());
        assert_eq!(h.0, vec!["execute_input", "stream", "execute_result"]);
        assert_eq!(r.outputs.len(), 2);
        assert_eq!(r.text(), "echo: hello\nHELLO\n");

        let r = k.execute("raise").await.unwrap();
        assert_eq!(r.error(), Some(("FakeError", "raised")));
        assert!(!k.owns_process());
        assert!(k.restart().await.is_err());
        k.shutdown().await.unwrap();
        k.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stdin_goes_through_the_handler() {
        let k = fake().await;
        let mut h = Collect(vec![], "forty-two");
        let opts = ExecuteOptions {
            allow_stdin: true,
            ..Default::default()
        };
        let r = k.execute_with("input", &opts, &mut h).await.unwrap();
        assert_eq!(h.0[..2], ["execute_input".to_string(), "input:? ".into()]);
        assert_eq!(r.text(), "you said: forty-two\n");
    }

    #[tokio::test]
    async fn interrupt_while_executing() {
        let k = Arc::new(fake().await);
        let k2 = k.clone();
        let run = tokio::spawn(async move { k2.execute("sleep 10000").await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        k.interrupt().await.unwrap();
        let r = run.await.unwrap().unwrap();
        assert_eq!(r.error().map(|e| e.0), Some("KeyboardInterrupt"));
    }

    #[tokio::test]
    async fn timeout_interrupts_the_cell() {
        let k = fake().await;
        let opts = ExecuteOptions {
            timeout: Some(Duration::from_millis(100)),
            ..Default::default()
        };
        assert!(k
            .execute_with("sleep 10000", &opts, &mut NoHandler)
            .await
            .is_err());
        // Without the interrupt the kernel would still be sleeping.
        let opts = ExecuteOptions {
            timeout: Some(Duration::from_secs(2)),
            ..Default::default()
        };
        let r = k
            .execute_with("hello", &opts, &mut NoHandler)
            .await
            .unwrap();
        assert!(r.is_ok());
    }
}
