//! A fake kernel over the in-process transport. It exists to test the
//! session state machine and to demonstrate the in-process seam the iPad
//! profile uses. It speaks the protocol, not any language.

use crate::transport::KernelSide;
use jcore_protocol::content::*;
use jcore_protocol::{Channel, Message};
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
}

impl FakeKernel {
    pub fn spawn(side: KernelSide, order: ReplyOrder) -> tokio::task::JoinHandle<()> {
        let k = FakeKernel {
            side,
            order,
            execution_count: 0,
            interrupted: false,
        };
        tokio::spawn(k.run())
    }

    async fn run(mut self) {
        while let Some((channel, msg)) = self.side.incoming.recv().await {
            match (channel, msg.msg_type()) {
                (Channel::Shell | Channel::Control, "kernel_info_request") => {
                    self.status(&msg, ExecutionState::Busy).await;
                    self.reply(channel, &msg, "kernel_info_reply", &kernel_info())
                        .await;
                    self.status(&msg, ExecutionState::Idle).await;
                }
                (Channel::Shell, "execute_request") => {
                    let req: ExecuteRequest = msg.content_as().unwrap();
                    self.execute(&msg, &req).await;
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

    async fn execute(&mut self, msg: &Message, req: &ExecuteRequest) {
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
                    Ok(None) => return,
                    _ => {}
                }
            }
            if self.interrupted {
                status = ReplyStatus::Error;
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
        } else if code == "raise" {
            status = ReplyStatus::Error;
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
                    None => return,
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

        let reply = ExecuteReply {
            status,
            execution_count: Some(n),
            ename: None,
            evalue: None,
            traceback: None,
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
