//! The host API for kernels (`docs/host-api.md`): cell code asks the
//! host for things by method name, with JSON, over a comm the host opens
//! to each kernel. The kernel holds no token: the host decides what each
//! kernel may call.
//!
//! [`attach`] joins a [`KernelSession`] once the kernel has answered. It
//! asks for a kernel subshell, so a cell awaiting a call still gets the
//! reply, and opens a comm to target [`TARGET`] on it, with the `hello`
//! reply as the open's data. A kernel with no such target closes the comm
//! and nothing else happens. A kernel that lists no `kernel subshells`
//! gets no comm, since a call awaited in a cell would never be answered.
//! A client that loads after the kernel starts may open the comm itself,
//! to the same target; the host takes it and answers `{ "hello": ... }`
//! on it. One kernel has one host comm.
//!
//! Calls come from the kernel as `comm_msg` on iopub, `{ id, method,
//! params }` or `{ id, cancel: true }`, and replies go back on shell, on
//! the subshell, as `{ id, result }` or `{ id, error: { code, message } }`
//! with the `device` API's error codes. A cancelled call gets no reply.
//!
//! The host keeps all of this from its other clients:
//! [`HostLink::is_host_message`] says which kernel messages to leave out
//! of what it forwards. The crate knows no language and no methods but
//! `hello`: a [`Responder`] answers the rest.

use async_trait::async_trait;
use ganymede_kernel_host::KernelSession;
use ganymede_protocol::{Channel, Message};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::AbortHandle;

/// The comm target the host opens, and a late client opens.
pub const TARGET: &str = "ganymede.host.v1";
/// The `protocol` `hello` reports.
pub const PROTOCOL: u32 = 1;
/// Calls one kernel may have in flight; more fail `failed`.
pub const MAX_CALLS: usize = 32;
/// How long the kernel has to answer `create_subshell_request`.
const SUBSHELL_TIMEOUT: Duration = Duration::from_secs(10);

/// The kernel a call comes from.
#[derive(Debug, Clone)]
pub struct Caller {
    pub kernel_id: String,
}

/// What `hello` reports beside the protocol and the host's version.
#[derive(Debug, Clone, Default)]
pub struct Hello {
    /// The calling kernel's notebook, as an API path.
    pub notebook: Option<String>,
    /// The methods this kernel may call, `hello` aside.
    pub methods: Vec<String>,
}

/// A failed call. `code` is one of the `device` API's: `unavailable`,
/// `denied`, `cancelled`, `timeout`, `failed`, `disconnected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub code: &'static str,
    pub message: String,
}

impl HostError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        HostError {
            code: "unavailable",
            message: message.into(),
        }
    }
    pub fn denied(message: impl Into<String>) -> Self {
        HostError {
            code: "denied",
            message: message.into(),
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        HostError {
            code: "failed",
            message: message.into(),
        }
    }
}

/// Answers a kernel's calls. Implemented by the server (and later the
/// engine), which know notebooks, rooms and grants.
#[async_trait]
pub trait Responder: Send + Sync + 'static {
    async fn hello(&self, caller: &Caller) -> Hello;
    /// Answer `method`. A method the responder doesn't have fails
    /// `unavailable`; one the kernel may not call fails `denied`.
    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, HostError>;
}

/// One kernel's host comm. Dropping it stops answering.
pub struct HostLink {
    shared: Arc<Shared>,
    task: AbortHandle,
}

struct Shared {
    /// The `header.session` of every message the host sends, so replies
    /// and statuses for them can be told apart.
    tag: String,
    /// Every host comm this kernel has had: the host's own, and each the
    /// kernel opened while the driver ran. Never shrinks, so a message on
    /// one is recognised whichever of the forwarder and the driver sees
    /// it first.
    comm_ids: Mutex<HashSet<String>>,
    /// The driver is answering: a kernel's open of [`TARGET`] is the
    /// host's. Before and after, it goes to clients, who close it.
    live: AtomicBool,
    /// Calls in flight, by the kernel's id for them.
    calls: Mutex<HashMap<String, AbortHandle>>,
}

impl Shared {
    fn is_host_comm(&self, msg: &Message) -> bool {
        msg.content["comm_id"]
            .as_str()
            .is_some_and(|id| self.comm_ids.lock().unwrap().contains(id))
    }

    fn abort_calls(&self) {
        for (_, call) in self.calls.lock().unwrap().drain() {
            call.abort();
        }
    }
}

impl Drop for HostLink {
    fn drop(&mut self) {
        self.task.abort();
        self.shared.live.store(false, Ordering::SeqCst);
        self.shared.abort_calls();
    }
}

impl HostLink {
    /// Whether a message from the kernel belongs to the host comm, and so
    /// stays out of what other clients get: replies and statuses for the
    /// host's own messages, the comm's traffic, and a kernel's open of
    /// [`TARGET`].
    pub fn is_host_message(&self, channel: Channel, msg: &Message) -> bool {
        if msg
            .parent_header
            .as_ref()
            .is_some_and(|p| p.session == self.shared.tag)
        {
            return true;
        }
        if channel != Channel::IoPub {
            return false;
        }
        match msg.msg_type() {
            // Recorded here, in the order clients would get it, so the
            // comm's first call can't reach them before the driver sees
            // the open.
            "comm_open" if msg.content["target_name"] == TARGET => {
                if !self.shared.live.load(Ordering::SeqCst) {
                    return false;
                }
                if let Some(id) = msg.content["comm_id"].as_str() {
                    self.shared.comm_ids.lock().unwrap().insert(id.to_string());
                }
                true
            }
            "comm_msg" | "comm_close" => self.shared.is_host_comm(msg),
            _ => false,
        }
    }
}

/// Start answering host calls on `session`, whose kernel has answered
/// `kernel_info_request`. `version` is the host's, for `hello`.
pub fn attach(
    session: &KernelSession,
    caller: Caller,
    responder: Arc<dyn Responder>,
    version: &str,
) -> HostLink {
    let shared = Arc::new(Shared {
        tag: format!("ganymede-host-{}", uuid::Uuid::new_v4().simple()),
        comm_ids: Mutex::default(),
        live: AtomicBool::new(false),
        calls: Mutex::default(),
    });
    // Subscribed before anything is sent, so nothing is missed.
    let rx = session.subscribe_lossless();
    let driver = Driver {
        session: session.clone(),
        shared: shared.clone(),
        caller,
        responder,
        version: version.to_string(),
        subshell: String::new(),
        current: Mutex::new(None),
    };
    let task = tokio::spawn(driver.run(rx)).abort_handle();
    HostLink { shared, task }
}

struct Driver {
    session: KernelSession,
    shared: Arc<Shared>,
    caller: Caller,
    responder: Arc<dyn Responder>,
    version: String,
    subshell: String,
    /// The comm calls are answered on, if one is open.
    current: Mutex<Option<String>>,
}

/// Marks the driver gone, however it ends.
struct Live(Arc<Shared>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.store(false, Ordering::SeqCst);
    }
}

impl Driver {
    async fn run(mut self, mut rx: tokio::sync::mpsc::UnboundedReceiver<(Channel, Message)>) {
        let subshells = self
            .session
            .kernel_info()
            .is_some_and(|i| i.supported_features.iter().any(|f| f == "kernel subshells"));
        if !subshells {
            tracing::debug!(kernel = %self.caller.kernel_id, "no kernel subshells: no host comm");
            return;
        }
        match self.create_subshell().await {
            Some(id) => self.subshell = id,
            None => return,
        }
        let comm_id = uuid::Uuid::new_v4().simple().to_string();
        self.shared.comm_ids.lock().unwrap().insert(comm_id.clone());
        *self.current.lock().unwrap() = Some(comm_id.clone());
        self.shared.live.store(true, Ordering::SeqCst);
        let _live = Live(self.shared.clone());
        let hello = self.hello().await;
        let open = json!({"comm_id": comm_id, "target_name": TARGET, "data": hello});
        if self.send("comm_open", &open).await.is_err() {
            return;
        }
        while let Some((channel, msg)) = rx.recv().await {
            if channel != Channel::IoPub {
                continue;
            }
            match msg.msg_type() {
                "comm_open" if msg.content["target_name"] == TARGET => self.opened(&msg).await,
                "comm_close" if self.is_ours(&msg) => {
                    // No target in the kernel, or the client closed it. A
                    // client may still open one later.
                    *self.current.lock().unwrap() = None;
                    self.shared.abort_calls();
                }
                "comm_msg" if self.is_ours(&msg) => self.message(&msg),
                _ => {}
            }
        }
    }

    fn is_ours(&self, msg: &Message) -> bool {
        let id = msg.content["comm_id"].as_str();
        id.is_some() && self.current.lock().unwrap().as_deref() == id
    }

    async fn create_subshell(&self) -> Option<String> {
        let msg = self.message_of("create_subshell_request", &json!({}));
        let reply = self
            .session
            .request(Channel::Control, msg, false, SUBSHELL_TIMEOUT)
            .await;
        match reply {
            Ok(e) => match e.reply.content["subshell_id"].as_str() {
                Some(id) if e.reply.content["status"] == "ok" => Some(id.to_string()),
                _ => {
                    tracing::warn!(kernel = %self.caller.kernel_id, "kernel refused a subshell: no host comm");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(kernel = %self.caller.kernel_id, "no subshell ({e}): no host comm");
                None
            }
        }
    }

    async fn hello(&self) -> Value {
        let h = self.responder.hello(&self.caller).await;
        json!({
            "protocol": PROTOCOL,
            "server": self.version,
            "notebook": h.notebook,
            "methods": h.methods,
        })
    }

    /// The kernel opened a host comm: take it if none is open, or turn it
    /// away.
    async fn opened(&self, msg: &Message) {
        let Some(id) = msg.content["comm_id"].as_str() else {
            return;
        };
        self.shared.comm_ids.lock().unwrap().insert(id.to_string());
        let taken = {
            let mut current = self.current.lock().unwrap();
            if current.is_some() {
                true
            } else {
                *current = Some(id.to_string());
                false
            }
        };
        if taken {
            let close = json!({"comm_id": id, "data": {"error": "taken"}});
            let _ = self.send("comm_close", &close).await;
            return;
        }
        let hello = self.hello().await;
        let _ = self
            .send(
                "comm_msg",
                &json!({"comm_id": id, "data": {"hello": hello}}),
            )
            .await;
    }

    /// A call or a cancel.
    fn message(&self, msg: &Message) {
        let data = &msg.content["data"];
        let id = match &data["id"] {
            Value::Null => return,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if data["cancel"] == true {
            if let Some(call) = self.shared.calls.lock().unwrap().remove(&id) {
                call.abort();
            }
            return;
        }
        let id_value = data["id"].clone();
        let method = data["method"].as_str().unwrap_or_default().to_string();
        let params = match &data["params"] {
            Value::Null => json!({}),
            p => p.clone(),
        };
        let comm_id = msg.content["comm_id"].clone();
        let sender = Sender {
            session: self.session.clone(),
            tag: self.shared.tag.clone(),
            subshell: self.subshell.clone(),
        };
        let mut calls = self.shared.calls.lock().unwrap();
        if calls.len() >= MAX_CALLS {
            let error = json!({"code": "failed", "message": format!("more than {MAX_CALLS} host calls in flight")});
            let reply = json!({"comm_id": comm_id, "data": {"id": id_value, "error": error}});
            tokio::spawn(async move { sender.send("comm_msg", &reply).await });
            return;
        }
        let responder = self.responder.clone();
        let caller = self.caller.clone();
        let hello = (method == "hello").then(|| (self.responder.clone(), self.version.clone()));
        let table = self.shared.clone();
        let key = id.clone();
        // Registered before the task can finish and remove itself.
        let (start, started) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = started.await;
            let answer = match hello {
                Some((r, version)) => {
                    let h = r.hello(&caller).await;
                    Ok(json!({"protocol": PROTOCOL, "server": version,
                              "notebook": h.notebook, "methods": h.methods}))
                }
                None => responder.call(&caller, &method, params).await,
            };
            // A cancel that came while the answer was being sent would
            // find nothing to abort; one that came before has aborted us.
            if table.calls.lock().unwrap().remove(&key).is_none() {
                return;
            }
            let data = match answer {
                Ok(result) => json!({"id": id_value, "result": result}),
                Err(e) => json!({"id": id_value, "error": {"code": e.code, "message": e.message}}),
            };
            sender
                .send("comm_msg", &json!({"comm_id": comm_id, "data": data}))
                .await;
        });
        if let Some(old) = calls.insert(id, task.abort_handle()) {
            old.abort();
        }
        drop(calls);
        let _ = start.send(());
    }

    fn message_of(&self, msg_type: &str, content: &Value) -> Message {
        Message::new(msg_type, &self.shared.tag, "ganymede", content)
    }

    /// Send on shell, on the host's subshell.
    async fn send(&self, msg_type: &str, content: &Value) -> ganymede_kernel_host::Result<()> {
        let mut msg = self.message_of(msg_type, content);
        msg.header.subshell_id = Some(self.subshell.clone());
        self.session.send(Channel::Shell, msg).await
    }
}

/// What a call's task needs to reply.
struct Sender {
    session: KernelSession,
    tag: String,
    subshell: String,
}

impl Sender {
    async fn send(&self, msg_type: &str, content: &Value) {
        let mut msg = Message::new(msg_type, &self.tag, "ganymede", content);
        msg.header.subshell_id = Some(self.subshell.clone());
        if let Err(e) = self.session.send(Channel::Shell, msg).await {
            tracing::debug!("host reply not sent: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
