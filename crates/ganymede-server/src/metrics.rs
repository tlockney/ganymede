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

/// How long a kernel that missed its probe is measured from the host
/// before it is asked again: a kernel busy in native code can miss one.
const REPROBE_AFTER: Duration = Duration::from_secs(60);

/// What a session's kernel did with `usage_request`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Answer {
    Answers,
    /// Replied with an error or something other than `usage_reply`.
    Refuses,
    /// Said nothing within the probe timeout, at this time.
    Missed(std::time::Instant),
}

pub struct Metrics {
    /// How long a kernel gets to answer its first `usage_request`. One
    /// that refuses is measured from the host until it restarts; one that
    /// says nothing, for a minute before it is asked again.
    pub probe_timeout: Duration,
    /// Kernel id to (session id, what that session's kernel did with
    /// `usage_request`). A restart starts a new session, which is asked
    /// again.
    answers: Mutex<HashMap<String, (String, Answer)>>,
    /// `None` where sysinfo could not start, which then measures nothing.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    system: Mutex<Option<Scans>>,
}

/// One `System` for every request, and when its processes and host
/// figures were last refreshed. A refresh sooner than sysinfo's minimum
/// CPU interval after the last is skipped: its CPU figures would be noise,
/// and requests close together (a client polling the server and its
/// kernels) share one process scan.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
struct Scans {
    sys: sysinfo::System,
    processes_at: Option<std::time::Instant>,
    host_at: Option<std::time::Instant>,
    /// Pids the last process scan was redone for and still lacked, so a
    /// gone kernel doesn't force a scan on every request.
    missing: Vec<u32>,
}

/// True, and `at` set to now, when the last refresh is old enough.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn due(at: &mut Option<std::time::Instant>) -> bool {
    let now = std::time::Instant::now();
    if at.is_some_and(|t| now.duration_since(t) < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL) {
        return false;
    }
    *at = Some(now);
    true
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            probe_timeout: Duration::from_secs(2),
            answers: Mutex::default(),
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            system: Mutex::new(sysinfo::System::new().ok().map(|mut sys| {
                // The first host CPU figure is then since now, not boot.
                sys.refresh_cpu_usage();
                Scans {
                    sys,
                    processes_at: None,
                    host_at: None,
                    missing: Vec::new(),
                }
            })),
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
        let mut scans = self.system.lock().unwrap();
        let scans = scans.as_mut()?;
        let root = Pid::from_u32(pid);
        // A process started since the last scan is not in it: scan again,
        // once per scan for each pid.
        let started_since =
            !scans.sys.processes().contains_key(&root) && !scans.missing.contains(&pid);
        if due(&mut scans.processes_at) || started_since {
            scans.processes_at = Some(std::time::Instant::now());
            scans.missing.clear();
            scans.sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing()
                    .with_memory()
                    .with_cpu()
                    .without_tasks(),
            );
        }
        if !scans.sys.processes().contains_key(&root) {
            scans.missing.push(pid);
            return None;
        }
        let processes = scans.sys.processes();
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (id, p) in processes {
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
        let mut scans = self.system.lock().unwrap();
        let Some(scans) = scans.as_mut() else { return };
        if due(&mut scans.host_at) {
            scans.sys.refresh_memory();
            scans.sys.refresh_cpu_usage();
        }
        let sys = &scans.sys;
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

    /// Whether to ask the kernel's current session: `Some(true)` when it
    /// answers, `Some(false)` when it refuses or missed a probe within the
    /// last minute, `None` when it is to be probed. Forgets kernels no
    /// longer running.
    fn answers(&self, kernels: &[String], id: &str, session: &str) -> Option<bool> {
        let mut answers = self.answers.lock().unwrap();
        answers.retain(|k, _| kernels.iter().any(|id| id == k));
        match answers.get(id) {
            Some((s, a)) if s == session => match a {
                Answer::Answers => Some(true),
                Answer::Refuses => Some(false),
                Answer::Missed(at) if at.elapsed() < REPROBE_AFTER => Some(false),
                Answer::Missed(_) => None,
            },
            _ => None,
        }
    }

    /// Record what the session did. A miss never overwrites an answer: a
    /// request that timed out while another got a reply (two first
    /// requests at once) says nothing new.
    fn set_answers(&self, id: &str, session: &str, answer: Answer) {
        let mut answers = self.answers.lock().unwrap();
        if matches!(answer, Answer::Missed(_))
            && matches!(answers.get(id), Some((s, Answer::Answers)) if s == session)
        {
            return;
        }
        answers.insert(id.to_string(), (session.to_string(), answer));
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
                m.set_answers(id, &sid, Answer::Answers);
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
            Ok(None) => m.set_answers(id, &sid, Answer::Refuses),
            Err(()) => m.set_answers(id, &sid, Answer::Missed(std::time::Instant::now())),
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
