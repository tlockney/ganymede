//! A fake kernel over the in-process transport. It exists to test the
//! session state machine and to demonstrate the in-process seam the iPad
//! profile uses. It speaks the protocol, not any language.
//!
//! Cells: `sleep <ms>` (interruptible), `raise`, `input`, `die` (the
//! process vanishes, nothing replies), `exit` (prints, then vanishes
//! mid-cell), `display` (display, update, clear, stream), `comm` (opens
//! a comm from the kernel side); anything else is echoed. Comms: target
//! `echo` sends every message back with its buffers; any other target is
//! closed at once, as ipykernel does. After an error with
//! `stop_on_error`, queued `execute_request`s are answered `aborted`.

use crate::transport::KernelSide;
use ganymede_protocol::content::*;
use ganymede_protocol::{Channel, Message};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::sync::mpsc;

/// Whether the fake sends `execute_reply` before or after `status: idle`.
/// Real kernels do both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyOrder {
    ReplyThenIdle,
    IdleThenReply,
}

pub struct FakeKernel {
    side: KernelSide,
    order: ReplyOrder,
    execution_count: u64,
    interrupted: bool,
    /// Open comms: id to target name.
    comms: HashMap<String, String>,
    /// Messages taken off the channel early (while aborting), handled next.
    backlog: VecDeque<(Channel, Message)>,
}

impl FakeKernel {
    pub fn spawn(side: KernelSide, order: ReplyOrder) -> tokio::task::JoinHandle<()> {
        let k = FakeKernel {
            side,
            order,
            execution_count: 0,
            interrupted: false,
            comms: HashMap::new(),
            backlog: VecDeque::new(),
        };
        tokio::spawn(k.run())
    }

    async fn recv(&mut self) -> Option<(Channel, Message)> {
        match self.backlog.pop_front() {
            Some(m) => Some(m),
            None => self.side.incoming.recv().await,
        }
    }

    async fn run(mut self) {
        while let Some((channel, msg)) = self.recv().await {
            match (channel, msg.msg_type()) {
                (Channel::Shell | Channel::Control, "kernel_info_request") => {
                    self.status(&msg, ExecutionState::Busy).await;
                    self.reply(channel, &msg, "kernel_info_reply", &kernel_info())
                        .await;
                    self.status(&msg, ExecutionState::Idle).await;
                }
                // The process dying mid-session: the transport closes and
                // nothing replies.
                (Channel::Shell, "execute_request") if msg.content["code"] == "die" => return,
                (Channel::Shell, "execute_request") if msg.content["code"] == "exit" => {
                    self.status(&msg, ExecutionState::Busy).await;
                    self.stream(&msg, "partial\n").await;
                    return;
                }
                (Channel::Shell, "execute_request") => {
                    let req: ExecuteRequest = msg.content_as().unwrap();
                    let failed = self.execute(&msg, &req).await;
                    if failed && req.stop_on_error {
                        self.abort_queued().await;
                    }
                }
                (Channel::Shell, "comm_open" | "comm_msg" | "comm_close") => {
                    self.status(&msg, ExecutionState::Busy).await;
                    self.comm(&msg).await;
                    self.status(&msg, ExecutionState::Idle).await;
                }
                (Channel::Shell, "is_complete_request") => {
                    // A trailing backslash continues the line.
                    let code = msg.content["code"].as_str().unwrap_or_default();
                    let status = if code.trim_end_matches('\n').ends_with('\\') {
                        serde_json::json!({"status": "incomplete", "indent": ""})
                    } else {
                        serde_json::json!({"status": "complete"})
                    };
                    self.reply(channel, &msg, "is_complete_reply", &status)
                        .await;
                }
                (Channel::Shell, "complete_request") => {
                    let code = msg.content["code"].as_str().unwrap_or_default();
                    let pos = msg.content["cursor_pos"].as_u64().unwrap_or(0) as usize;
                    let word: String = code.chars().take(pos).collect();
                    let start = word.rfind(' ').map(|i| i + 1).unwrap_or(0);
                    let prefix = &word[start..];
                    let matches: Vec<&str> = ["echo", "input", "raise", "sleep"]
                        .into_iter()
                        .filter(|w| w.starts_with(prefix))
                        .collect();
                    let reply = serde_json::json!({"status": "ok", "matches": matches,
                        "cursor_start": word[..start].chars().count(), "cursor_end": pos, "metadata": {}});
                    self.reply(channel, &msg, "complete_reply", &reply).await;
                }
                (Channel::Shell, "inspect_request") => {
                    let reply = serde_json::json!({"status": "ok", "found": false, "data": {}, "metadata": {}});
                    self.reply(channel, &msg, "inspect_reply", &reply).await;
                }
                (Channel::Shell, "comm_info_request") => {
                    let want = msg.content["target_name"].as_str();
                    let comms: serde_json::Map<String, serde_json::Value> = self
                        .comms
                        .iter()
                        .filter(|(_, t)| want.is_none_or(|w| w == t.as_str()))
                        .map(|(id, t)| (id.clone(), serde_json::json!({ "target_name": t })))
                        .collect();
                    let reply = serde_json::json!({"status": "ok", "comms": comms});
                    self.reply(channel, &msg, "comm_info_reply", &reply).await;
                }
                (Channel::Control, "interrupt_request") => {
                    self.interrupted = true;
                    self.reply(
                        channel,
                        &msg,
                        "interrupt_reply",
                        &serde_json::json!({"status": "ok"}),
                    )
                    .await;
                }
                (Channel::Control, "shutdown_request") => {
                    let req: ShutdownRequest = msg.content_as().unwrap();
                    self.reply(
                        channel,
                        &msg,
                        "shutdown_reply",
                        &ShutdownReply {
                            status: ReplyStatus::Ok,
                            restart: req.restart,
                            extra: Default::default(),
                        },
                    )
                    .await;
                    return;
                }
                _ => {}
            }
        }
    }

    /// Answer every `execute_request` already queued with `aborted`, as a
    /// kernel does after an error under `stop_on_error`. Anything else
    /// queued is kept for later.
    async fn abort_queued(&mut self) {
        while let Ok((channel, m)) = self.side.incoming.try_recv() {
            if channel == Channel::Shell && m.msg_type() == "execute_request" {
                self.status(&m, ExecutionState::Busy).await;
                let reply = serde_json::json!({"status": "aborted", "execution_count": null});
                self.reply(Channel::Shell, &m, "execute_reply", &reply)
                    .await;
                self.status(&m, ExecutionState::Idle).await;
            } else {
                self.backlog.push_back((channel, m));
            }
        }
    }

    async fn comm(&mut self, msg: &Message) {
        let id = msg.content["comm_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match msg.msg_type() {
            "comm_open" => {
                let target = msg.content["target_name"].as_str().unwrap_or_default();
                if target == "echo" {
                    self.comms.insert(id, target.into());
                } else {
                    let close = serde_json::json!({"comm_id": id, "data": {}});
                    self.iopub(msg, "comm_close", &close).await;
                }
            }
            "comm_msg" if self.comms.contains_key(&id) => {
                let echo = serde_json::json!({"comm_id": id, "data": msg.content["data"]});
                let m = Message::reply_to(msg, "comm_msg", &echo).with_buffers(msg.buffers.clone());
                let _ = self.side.outgoing.send((Channel::IoPub, m)).await;
            }
            "comm_close" => {
                self.comms.remove(&id);
            }
            _ => {}
        }
    }

    async fn stream(&self, parent: &Message, text: &str) {
        self.iopub(
            parent,
            "stream",
            &Stream {
                name: "stdout".into(),
                text: text.into(),
                extra: Default::default(),
            },
        )
        .await;
    }

    /// Run a cell; `true` when it failed.
    async fn execute(&mut self, msg: &Message, req: &ExecuteRequest) -> bool {
        self.execution_count += 1;
        let n = self.execution_count;
        self.status(msg, ExecutionState::Busy).await;
        self.iopub(
            msg,
            "execute_input",
            &ExecuteInput {
                code: req.code.clone(),
                execution_count: Some(n),
                extra: Default::default(),
            },
        )
        .await;

        let code = req.code.trim();
        let mut status = ReplyStatus::Ok;
        let mut error: Option<(&str, &str)> = None;
        if let Some(ms) = code.strip_prefix("sleep ") {
            let ms: u64 = ms.parse().unwrap_or(0);
            let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
            self.interrupted = false;
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                // Poll the control channel for an interrupt while "busy".
                match tokio::time::timeout(Duration::from_millis(10), self.side.incoming.recv())
                    .await
                {
                    Ok(Some((Channel::Control, m))) if m.msg_type() == "interrupt_request" => {
                        self.reply(
                            Channel::Control,
                            &m,
                            "interrupt_reply",
                            &serde_json::json!({"status": "ok"}),
                        )
                        .await;
                        self.interrupted = true;
                        break;
                    }
                    // A real kernel's control thread answers while a
                    // cell runs, publishing statuses of its own.
                    Ok(Some((Channel::Control, m))) if m.msg_type() == "kernel_info_request" => {
                        self.status(&m, ExecutionState::Busy).await;
                        self.reply(Channel::Control, &m, "kernel_info_reply", &kernel_info())
                            .await;
                        self.status(&m, ExecutionState::Idle).await;
                    }
                    Ok(None) => return true,
                    _ => {}
                }
            }
            if self.interrupted {
                status = ReplyStatus::Error;
                error = Some(("KeyboardInterrupt", ""));
                self.iopub(
                    msg,
                    "error",
                    &ErrorContent {
                        ename: "KeyboardInterrupt".into(),
                        evalue: String::new(),
                        traceback: vec![],
                        extra: Default::default(),
                    },
                )
                .await;
            }
        } else if code == "display" {
            let md = |text: &str| serde_json::json!({ "text/markdown": text });
            let transient = serde_json::json!({ "display_id": "d1" });
            self.iopub(
                msg,
                "display_data",
                &serde_json::json!({"data": md("one"), "metadata": {}, "transient": transient}),
            )
            .await;
            self.iopub(
                msg,
                "update_display_data",
                &serde_json::json!({"data": md("two"), "metadata": {}, "transient": transient}),
            )
            .await;
            self.iopub(msg, "clear_output", &serde_json::json!({"wait": true}))
                .await;
            self.stream(msg, "after\n").await;
        } else if code == "comm" {
            let id = uuid::Uuid::new_v4().simple().to_string();
            self.comms.insert(id.clone(), "fake.target".into());
            let open = serde_json::json!({"comm_id": id, "target_name": "fake.target", "data": {}});
            self.iopub(msg, "comm_open", &open).await;
        } else if code == "raise" {
            status = ReplyStatus::Error;
            error = Some(("FakeError", "raised"));
            self.iopub(
                msg,
                "error",
                &ErrorContent {
                    ename: "FakeError".into(),
                    evalue: "raised".into(),
                    traceback: vec!["\u{1b}[31mFakeError\u{1b}[0m: raised".into()],
                    extra: Default::default(),
                },
            )
            .await;
        } else if code == "input" {
            self.stdin(
                msg,
                "input_request",
                &InputRequest {
                    prompt: "? ".into(),
                    password: false,
                    extra: Default::default(),
                },
            )
            .await;
            let value = loop {
                match self.side.incoming.recv().await {
                    Some((Channel::Stdin, m)) if m.msg_type() == "input_reply" => {
                        break m.content_as::<InputReply>().unwrap().value;
                    }
                    Some(_) => {}
                    None => return true,
                }
            };
            self.iopub(
                msg,
                "stream",
                &Stream {
                    name: "stdout".into(),
                    text: format!("you said: {value}\n"),
                    extra: Default::default(),
                },
            )
            .await;
        } else {
            self.iopub(
                msg,
                "stream",
                &Stream {
                    name: "stdout".into(),
                    text: format!("echo: {code}\n"),
                    extra: Default::default(),
                },
            )
            .await;
            let mut data = MimeBundle::new();
            data.insert(
                "text/plain".into(),
                serde_json::Value::String(code.to_uppercase()),
            );
            self.iopub(
                msg,
                "execute_result",
                &ExecuteResult {
                    execution_count: Some(n),
                    data,
                    metadata: Default::default(),
                    extra: Default::default(),
                },
            )
            .await;
        }

        let failed = matches!(status, ReplyStatus::Error);
        let reply = ExecuteReply {
            status,
            execution_count: Some(n),
            ename: error.map(|e| e.0.to_string()),
            evalue: error.map(|e| e.1.to_string()),
            traceback: error.map(|_| vec![]),
            user_expressions: Default::default(),
            extra: Default::default(),
        };
        match self.order {
            ReplyOrder::ReplyThenIdle => {
                self.reply(Channel::Shell, msg, "execute_reply", &reply)
                    .await;
                self.status(msg, ExecutionState::Idle).await;
            }
            ReplyOrder::IdleThenReply => {
                self.status(msg, ExecutionState::Idle).await;
                self.reply(Channel::Shell, msg, "execute_reply", &reply)
                    .await;
            }
        }
        failed
    }

    async fn status(&self, parent: &Message, state: ExecutionState) {
        self.iopub(
            parent,
            "status",
            &Status {
                execution_state: state,
                extra: Default::default(),
            },
        )
        .await;
    }

    async fn iopub<C: serde::Serialize>(&self, parent: &Message, msg_type: &str, content: &C) {
        let _ = self
            .side
            .outgoing
            .send((Channel::IoPub, Message::reply_to(parent, msg_type, content)))
            .await;
    }

    async fn stdin<C: serde::Serialize>(&self, parent: &Message, msg_type: &str, content: &C) {
        let _ = self
            .side
            .outgoing
            .send((Channel::Stdin, Message::reply_to(parent, msg_type, content)))
            .await;
    }

    async fn reply<C: serde::Serialize>(
        &self,
        channel: Channel,
        parent: &Message,
        msg_type: &str,
        content: &C,
    ) {
        let _ = self
            .side
            .outgoing
            .send((channel, Message::reply_to(parent, msg_type, content)))
            .await;
    }
}

/// The fake's `kernel_info_reply`, with an extra top-level field to prove
/// unknown fields pass through the host.
pub fn kernel_info() -> KernelInfoReply {
    let mut extra = Extra::new();
    extra.insert(
        "capabilities".into(),
        serde_json::json!({"jit": false, "subprocess": false}),
    );
    KernelInfoReply {
        status: ReplyStatus::Ok,
        protocol_version: "5.5".into(),
        implementation: "fake".into(),
        implementation_version: "0".into(),
        language_info: LanguageInfo {
            name: "fake".into(),
            version: "0".into(),
            mimetype: "text/plain".into(),
            file_extension: ".txt".into(),
            codemirror_mode: serde_json::Value::String("text".into()),
            pygments_lexer: None,
            nbconvert_exporter: None,
            extra: Default::default(),
        },
        banner: "fake kernel".into(),
        help_links: vec![],
        debugger: None,
        supported_features: vec![],
        extra,
    }
}

// Keep the mpsc import used when the fake is compiled without tests.
#[allow(dead_code)]
fn _unused(_: mpsc::Sender<()>) {}
