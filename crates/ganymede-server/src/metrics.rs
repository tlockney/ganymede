//! Memory and CPU use in jupyter-resource-usage's REST shape, so clients
//! written for it work unchanged:
//!
//! - `GET /api/metrics/v1`: the server process and its descendants, which
//!   include subprocess kernels.
//! - `GET /api/metrics/v1/kernel_usage/get_usage/{kernel_id}`: one kernel.
//!   A kernel that answers `usage_request` on the control channel (as
//!   ipykernel does) has its `usage_reply` passed through, since its
//!   memory may not be in its own process. Otherwise the host measures the
//!   kernel's process and its descendants, and a kernel with no process
//!   the host can measure (attached, in-process) gets
//!   `{"content": {"reason": "no_process"}}`.

use crate::routes::HttpError;
use crate::AppState;
use axum::extract::{Path, State};
use axum::Json;
use ganymede_kernel_host::KernelSession;
use ganymede_protocol::content::ReplyStatus;
use ganymede_protocol::Channel;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a kernel known to answer `usage_request` gets, as
/// jupyter-resource-usage waits.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Metrics {
    /// How long a kernel gets to answer its first `usage_request`. One
    /// that doesn't is measured from the host until it restarts.
    pub probe_timeout: Duration,
    /// Kernel id to (session id, whether that session's kernel answers
    /// `usage_request`). A restart starts a new session, which is asked
    /// again.
    answers: Mutex<HashMap<String, (String, bool)>>,
    /// `None` where sysinfo could not start, which then measures nothing.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    system: Mutex<Option<sysinfo::System>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            probe_timeout: Duration::from_secs(2),
            answers: Mutex::default(),
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            system: Mutex::new(sysinfo::System::new().ok()),
        }
    }
}

/// A process and its descendants.
#[derive(Debug, Clone, Copy)]
struct Usage {
    /// Resident memory, in bytes.
    rss: u64,
    /// Percent of one CPU since the previous measurement, so over 100 on
    /// several cores, and 0 the first time.
    cpu: f64,
}

impl Metrics {
    /// Metrics that give a kernel `probe` to answer its first `usage_request`.
    pub fn with_probe_timeout(probe: Duration) -> Self {
        Metrics {
            probe_timeout: probe,
            ..Default::default()
        }
    }

    /// Memory and CPU of `pid` and its descendants; `None` when the
    /// process is gone or this platform cannot measure it.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn measure(&self, pid: u32) -> Option<Usage> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};
        let mut sys = self.system.lock().unwrap();
        let sys = sys.as_mut()?;
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let processes = sys.processes();
        let root = Pid::from_u32(pid);
        processes.get(&root)?;
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (id, p) in processes {
            // Linux lists threads too; they share their process's memory.
            if p.thread_kind().is_some() {
                continue;
            }
            if let Some(parent) = p.parent() {
                children.entry(parent).or_default().push(*id);
            }
        }
        let mut usage = Usage { rss: 0, cpu: 0.0 };
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Some(p) = processes.get(&id) {
                usage.rss += p.memory();
                usage.cpu += f64::from(p.cpu_usage());
            }
            stack.extend(children.remove(&id).unwrap_or_default());
        }
        Some(usage)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn measure(&self, _pid: u32) -> Option<Usage> {
        None
    }

    /// The machine's CPU and memory, in `usage_reply`'s field names.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn host(&self, content: &mut serde_json::Map<String, Value>) {
        let mut sys = self.system.lock().unwrap();
        let Some(sys) = sys.as_mut() else { return };
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        if let Ok(name) = sysinfo::System::host_name() {
            content.insert("hostname".into(), name.into());
        }
        // 0 on the first measurement, which ipykernel leaves out too.
        let cpu = sys.global_cpu_usage();
        if cpu > 0.0 {
            content.insert("host_cpu_percent".into(), f64::from(cpu).into());
        }
        let total = sys.total_memory();
        let available = sys.available_memory();
        let percent = if total == 0 {
            0.0
        } else {
            (total - available.min(total)) as f64 * 100.0 / total as f64
        };
        content.insert(
            "host_virtual_memory".into(),
            json!({
                "total": total,
                "available": available,
                "percent": percent,
                "used": sys.used_memory(),
                "free": sys.free_memory(),
            }),
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn host(&self, _content: &mut serde_json::Map<String, Value>) {}

    /// Whether the kernel's current session answers `usage_request`, if
    /// known. Forgets kernels no longer running.
    fn answers(&self, kernels: &[String], id: &str, session: &str) -> Option<bool> {
        let mut answers = self.answers.lock().unwrap();
        answers.retain(|k, _| kernels.iter().any(|id| id == k));
        match answers.get(id) {
            Some((s, yes)) if s == session => Some(*yes),
            _ => None,
        }
    }

    fn set_answers(&self, id: &str, session: &str, yes: bool) {
        self.answers
            .lock()
            .unwrap()
            .insert(id.to_string(), (session.to_string(), yes));
    }

    /// Ask the kernel for its `usage_reply`. `Ok(None)` when it does not
    /// answer, or answers with an error.
    async fn ask(&self, session: &KernelSession, timeout: Duration) -> Result<Option<Value>, ()> {
        let msg = session.message("usage_request", &json!({}));
        match session.request(Channel::Control, msg, false, timeout).await {
            Ok(exec)
                if exec.reply.msg_type() == "usage_reply"
                    && exec.reply_status() != Some(ReplyStatus::Error) =>
            {
                let mut reply = serde_json::to_value(&exec.reply).map_err(|_| ())?;
                reply["msg_id"] = exec.reply.msg_id().into();
                reply["msg_type"] = exec.reply.msg_type().into();
                Ok(Some(reply))
            }
            Ok(_) => Ok(None),
            Err(ganymede_kernel_host::Error::Timeout(_)) => Err(()),
            Err(_) => Ok(None),
        }
    }
}

fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn no_usage(id: &str, content: Value) -> Value {
    json!({ "content": content, "kernel_id": id })
}

/// `GET /api/metrics/v1`, as jupyter-resource-usage answers with its
/// defaults (no memory limit) and CPU tracking on.
pub async fn server(State(s): State<AppState>) -> Json<Value> {
    let m = s.metrics.clone();
    let usage = tokio::task::spawn_blocking(move || m.measure(std::process::id()))
        .await
        .ok()
        .flatten();
    let usage = usage.unwrap_or(Usage { rss: 0, cpu: 0.0 });
    Json(json!({
        "rss": usage.rss,
        "limits": { "memory": { "rss": 0, "pss": 0 } },
        "cpu_percent": usage.cpu,
        "cpu_count": cpu_count(),
    }))
}

/// `GET /api/metrics/v1/kernel_usage/get_usage/{kernel_id}`.
pub async fn kernel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, HttpError> {
    // A kernel still starting may not answer yet; asking it then would
    // mark it as one that never answers.
    s.kernels.wait_started(&id).await?;
    let session = s.kernels.get(&id)?.session();
    let running: Vec<String> = s.kernels.list().into_iter().map(|k| k.id).collect();
    Ok(Json(
        kernel_usage(&s.metrics, &running, &id, &session).await,
    ))
}

async fn kernel_usage(
    m: &Arc<Metrics>,
    running: &[String],
    id: &str,
    session: &KernelSession,
) -> Value {
    let sid = session.session_id().to_string();
    let known = m.answers(running, id, &sid);
    if known != Some(false) {
        let timeout = if known == Some(true) {
            ANSWER_TIMEOUT
        } else {
            m.probe_timeout
        };
        match m.ask(session, timeout).await {
            Ok(Some(mut reply)) => {
                m.set_answers(id, &sid, true);
                reply["kernel_id"] = id.into();
                if let Some(content) = reply["content"].as_object_mut() {
                    content.insert("host_usage_flag".into(), true.into());
                }
                return reply;
            }
            Err(()) if known == Some(true) => {
                let ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
                return no_usage(id, json!({ "reason": "timeout", "timeout_ms": ms }));
            }
            Ok(None) | Err(()) => m.set_answers(id, &sid, false),
        }
    }
    let Some(pid) = session.pid() else {
        return no_usage(id, json!({ "reason": "no_process" }));
    };
    let m = m.clone();
    let content = tokio::task::spawn_blocking(move || {
        let usage = m.measure(pid)?;
        let mut content = serde_json::Map::new();
        content.insert("pid".into(), pid.into());
        content.insert("kernel_cpu".into(), usage.cpu.into());
        content.insert("kernel_memory".into(), usage.rss.into());
        content.insert("cpu_count".into(), cpu_count().into());
        m.host(&mut content);
        content.insert("host_usage_flag".into(), true.into());
        Some(Value::Object(content))
    })
    .await
    .ok()
    .flatten();
    no_usage(
        id,
        content.unwrap_or_else(|| json!({ "reason": "no_process" })),
    )
}
