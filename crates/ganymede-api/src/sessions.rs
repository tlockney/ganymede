//! Jupyter "sessions": a notebook path bound to a kernel.

use crate::{ApiError, KernelManager, NotebookRef, Result, SessionModel};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
struct Session {
    id: String,
    path: String,
    name: String,
    type_: String,
    kernel_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KernelRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/sessions` body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCreate {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    #[serde(default)]
    pub kernel: KernelRef,
}

/// `PATCH /api/sessions/{id}` body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionPatch {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    #[serde(default)]
    pub kernel: Option<KernelRef>,
}

pub struct SessionManager {
    kernels: Arc<KernelManager>,
    sessions: RwLock<HashMap<String, Session>>,
}

impl SessionManager {
    pub fn new(kernels: Arc<KernelManager>) -> Self {
        SessionManager {
            kernels,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    pub fn kernels(&self) -> &Arc<KernelManager> {
        &self.kernels
    }

    fn model(&self, s: &Session) -> Result<SessionModel> {
        let kernel = self.kernels.model(&s.kernel_id)?;
        Ok(SessionModel {
            id: s.id.clone(),
            path: s.path.clone(),
            name: s.name.clone(),
            type_: s.type_.clone(),
            kernel,
            notebook: (s.type_ == "notebook").then(|| NotebookRef {
                path: s.path.clone(),
                name: s.name.clone(),
            }),
        })
    }

    /// Sessions whose kernel died are dropped here, as `jupyter_server`
    /// does ("Kernel culled or died unexpectedly").
    pub fn list(&self) -> Vec<SessionModel> {
        let mut sessions = self.sessions.write().unwrap();
        sessions.retain(|_, s| self.kernels.get(&s.kernel_id).is_ok());
        let mut v: Vec<_> = sessions
            .values()
            .filter_map(|s| self.model(s).ok())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn get(&self, id: &str) -> Result<SessionModel> {
        let s = self
            .sessions
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("session {id}")))?;
        self.model(&s)
    }

    /// Create, or return the existing session for the same path, as
    /// `jupyter_server` does.
    pub async fn create(&self, req: SessionCreate) -> Result<SessionModel> {
        if let Some(existing) = self
            .sessions
            .read()
            .unwrap()
            .values()
            .find(|s| s.path == req.path)
            .cloned()
        {
            return self.model(&existing);
        }
        let kernel_id = match req.kernel.id {
            Some(id) => {
                self.kernels.get(&id)?;
                id
            }
            None => {
                self.kernels
                    .start(req.kernel.name.as_deref(), Some(&req.path))
                    .await?
                    .id
            }
        };
        let s = Session {
            id: uuid::Uuid::new_v4().to_string(),
            name: req.name.unwrap_or_else(|| basename(&req.path)),
            type_: req.type_.unwrap_or_else(|| "notebook".into()),
            path: req.path,
            kernel_id,
        };
        self.sessions
            .write()
            .unwrap()
            .insert(s.id.clone(), s.clone());
        self.model(&s)
    }

    pub async fn update(&self, id: &str, patch: SessionPatch) -> Result<SessionModel> {
        let mut s = self
            .sessions
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("session {id}")))?;
        if let Some(p) = patch.path {
            s.path = p;
        }
        if let Some(n) = patch.name {
            s.name = n;
        }
        if let Some(t) = patch.type_ {
            s.type_ = t;
        }
        if let Some(k) = patch.kernel {
            let new_id = match (k.id, k.name) {
                (Some(id), _) => {
                    self.kernels.get(&id)?;
                    id
                }
                (None, Some(name)) => self.kernels.start(Some(&name), Some(&s.path)).await?.id,
                (None, None) => s.kernel_id.clone(),
            };
            if new_id != s.kernel_id {
                let _ = self.kernels.shutdown(&s.kernel_id).await;
                s.kernel_id = new_id;
            }
        }
        self.sessions
            .write()
            .unwrap()
            .insert(id.to_string(), s.clone());
        self.model(&s)
    }

    /// Delete the session and shut down its kernel.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let s = self
            .sessions
            .write()
            .unwrap()
            .remove(id)
            .ok_or_else(|| ApiError::not_found(format!("session {id}")))?;
        let _ = self.kernels.shutdown(&s.kernel_id).await;
        Ok(())
    }
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernels::{KernelManagerConfig, StaticKernelspecs, TransportFactory};
    use async_trait::async_trait;
    use ganymede_kernel_host::protocol::{
        content::ExecuteRequest, Channel, ConnectionInfo, Message,
    };
    use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
    use ganymede_kernel_host::{
        attach, in_process_pair, KernelTransport, Kernelspec, LaunchOptions, LaunchedKernel,
        Provisioner,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    struct FakeProvisioner;
    #[async_trait]
    impl Provisioner for FakeProvisioner {
        async fn launch(
            &self,
            _spec: &Kernelspec,
            _opts: LaunchOptions,
        ) -> ganymede_kernel_host::Result<LaunchedKernel> {
            Ok(attach(ConnectionInfo::new_tcp(
                "127.0.0.1",
                [1, 2, 3, 4, 5],
            )))
        }
    }

    /// What the next launches do. Once the script runs out, kernels start.
    #[derive(Clone, Copy)]
    enum Launch {
        /// The transport fails to connect.
        Fail,
        /// The kernel exits before it answers, as the loser of a port
        /// collision does.
        Die,
    }

    #[derive(Default)]
    struct FakeTransports {
        script: Mutex<VecDeque<Launch>>,
    }

    #[async_trait]
    impl TransportFactory for FakeTransports {
        async fn connect(
            &self,
            _launched: &LaunchedKernel,
        ) -> ganymede_kernel_host::Result<KernelTransport> {
            let (host, side) = in_process_pair(256);
            match self.script.lock().unwrap().pop_front() {
                None => {
                    FakeKernel::spawn(side, ReplyOrder::ReplyThenIdle);
                }
                Some(Launch::Die) => drop(side),
                Some(Launch::Fail) => return Err(ganymede_kernel_host::Error::TransportClosed),
            }
            Ok(host)
        }
    }

    fn fake_spec(name: &str) -> Kernelspec {
        Kernelspec {
            name: name.into(),
            resource_dir: "/nonexistent".into(),
            argv: vec!["fake".into()],
            display_name: format!("Fake {name}"),
            language: "fake".into(),
            interrupt_mode: ganymede_kernel_host::InterruptMode::Message,
            env: Default::default(),
            metadata: serde_json::json!({"capabilities": {"jit": false}})
                .as_object()
                .unwrap()
                .clone(),
            extra: Default::default(),
        }
    }

    pub(crate) fn manager(cull: Option<Duration>) -> Arc<KernelManager> {
        scripted(cull).0
    }

    fn scripted(cull: Option<Duration>) -> (Arc<KernelManager>, Arc<FakeTransports>) {
        let transports = Arc::new(FakeTransports::default());
        let m = Arc::new(KernelManager::new(
            Arc::new(StaticKernelspecs(vec![
                fake_spec("fake"),
                fake_spec("other"),
            ])),
            Arc::new(FakeProvisioner),
            transports.clone(),
            KernelManagerConfig {
                startup_timeout: Duration::from_secs(5),
                cull_idle: cull,
                ..Default::default()
            },
        ));
        (m, transports)
    }

    #[tokio::test]
    async fn kernelspecs_model_has_default_and_metadata() {
        let m = manager(None);
        let specs = m.kernelspecs().unwrap();
        assert_eq!(specs.default, "fake");
        assert_eq!(
            specs.kernelspecs["other"].spec.metadata["capabilities"]["jit"],
            serde_json::Value::Bool(false)
        );
        assert_eq!(specs.kernelspecs["fake"].spec.interrupt_mode, "message");
    }

    #[tokio::test]
    async fn kernel_lifecycle_and_models() {
        let m = manager(None);
        let k = m.start(None, Some("nb/notebook.ipynb")).await.unwrap();
        assert_eq!(
            (k.name.as_str(), k.execution_state.as_str(), k.connections),
            ("fake", "idle", 0)
        );
        assert_eq!(m.list().len(), 1);
        m.interrupt(&k.id).await.unwrap();
        let r = m.restart(&k.id).await.unwrap();
        assert_eq!(
            (r.id.as_str(), r.execution_state.as_str()),
            (k.id.as_str(), "idle")
        );
        m.shutdown(&k.id).await.unwrap();
        assert!(m.list().is_empty());
        assert_eq!(m.get(&k.id).unwrap_err().status, 404);
        assert_eq!(m.start(Some("nope"), None).await.unwrap_err().status, 404);
    }

    #[tokio::test]
    async fn clients_see_iopub_and_only_their_own_replies() {
        let m = manager(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let mut a = m.connect_client(&k.id).unwrap();
        let mut b = m.connect_client(&k.id).unwrap();
        assert_eq!(m.model(&k.id).unwrap().connections, 2);

        // Each client announces itself with a kernel_info_request so the
        // bridge learns its session id.
        for (c, sess) in [(&mut a, "sess-a"), (&mut b, "sess-b")] {
            c.send(
                Channel::Shell,
                Message::new("kernel_info_request", sess, "u", &serde_json::json!({})),
            )
            .await
            .unwrap();
        }
        a.send(
            Channel::Shell,
            Message::new("execute_request", "sess-a", "u", &ExecuteRequest::new("hi")),
        )
        .await
        .unwrap();

        let mut a_shell = 0;
        let mut a_iopub = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline && a_shell < 2 {
            match tokio::time::timeout(Duration::from_millis(200), a.recv()).await {
                Ok(Some((Channel::Shell, _))) => a_shell += 1,
                Ok(Some((Channel::IoPub, _))) => a_iopub += 1,
                _ => {}
            }
        }
        assert_eq!(a_shell, 2, "a gets its kernel_info_reply and execute_reply");
        assert!(a_iopub >= 5);

        let mut b_shell_types = vec![];
        let mut b_iopub = 0;
        while let Ok(Some((ch, msg))) =
            tokio::time::timeout(Duration::from_millis(300), b.recv()).await
        {
            match ch {
                Channel::Shell => b_shell_types.push(msg.msg_type().to_string()),
                Channel::IoPub => b_iopub += 1,
                _ => {}
            }
        }
        assert_eq!(
            b_shell_types,
            vec!["kernel_info_reply"],
            "b never sees a's execute_reply"
        );
        assert!(b_iopub >= 5, "b sees a's iopub output");

        drop(a);
        assert_eq!(m.model(&k.id).unwrap().connections, 1);
        m.shutdown(&k.id).await.unwrap();
        assert!(
            b.recv().await.is_none(),
            "hub closes when the kernel is gone"
        );
    }

    #[tokio::test]
    async fn restart_announces_restarting_to_connected_clients() {
        let m = manager(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let mut c = m.connect_client(&k.id).unwrap();
        m.restart(&k.id).await.unwrap();
        let mut states = vec![];
        while let Ok(Some((ch, msg))) =
            tokio::time::timeout(Duration::from_millis(300), c.recv()).await
        {
            if ch == Channel::IoPub && msg.msg_type() == "status" {
                states.push(msg.content["execution_state"].as_str().unwrap().to_string());
            }
        }
        assert!(states.contains(&"restarting".to_string()), "{states:?}");
        assert_eq!(states.last().map(String::as_str), Some("idle"));
    }

    #[tokio::test]
    async fn cull_shuts_down_idle_unconnected_kernels() {
        let m = manager(Some(Duration::from_millis(50)));
        let k = m.start(Some("fake"), None).await.unwrap();
        let busy = m.start(Some("fake"), None).await.unwrap();
        let _c = m.connect_client(&busy.id).unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(m.cull_idle().await, vec![k.id.clone()]);
        assert_eq!(m.list().len(), 1);
    }

    #[tokio::test]
    async fn sessions_create_reuse_patch_delete() {
        let m = manager(None);
        let sm = SessionManager::new(m.clone());
        let s = sm
            .create(SessionCreate {
                path: "a/b.ipynb".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            (s.name.as_str(), s.type_.as_str(), s.kernel.name.as_str()),
            ("b.ipynb", "notebook", "fake")
        );
        assert_eq!(s.notebook.as_ref().unwrap().path, "a/b.ipynb");
        let again = sm
            .create(SessionCreate {
                path: "a/b.ipynb".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(again.id, s.id);
        assert_eq!(m.list().len(), 1);

        let p = sm
            .update(
                &s.id,
                SessionPatch {
                    path: Some("c.ipynb".into()),
                    kernel: Some(KernelRef {
                        name: Some("other".into()),
                        id: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (p.path.as_str(), p.kernel.name.as_str()),
            ("c.ipynb", "other")
        );
        assert_eq!(m.list().len(), 1, "old kernel shut down on kernel change");

        sm.delete(&s.id).await.unwrap();
        assert!(m.list().is_empty());
        assert_eq!(sm.get(&s.id).unwrap_err().status, 404);
    }

    async fn until_none(c: &mut crate::bridge::ClientConnection) -> Vec<(Channel, Message)> {
        let mut got = vec![];
        while let Some(m) = tokio::time::timeout(Duration::from_secs(2), c.recv())
            .await
            .expect("client closes")
        {
            got.push(m);
        }
        got
    }

    #[tokio::test]
    async fn shut_down_kernels_are_freed() {
        let m = manager(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let weak = Arc::downgrade(&m.get(&k.id).unwrap());
        m.restart(&k.id).await.unwrap();
        let mut c = m.connect_client(&k.id).unwrap();
        m.shutdown(&k.id).await.unwrap();
        until_none(&mut c).await;
        drop(c);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(weak.upgrade().is_none(), "RunningKernel leaked");
    }

    #[tokio::test]
    async fn kernel_that_dies_is_reported_dead_and_removed() {
        let m = manager(None);
        let sm = SessionManager::new(m.clone());
        let s = sm
            .create(SessionCreate {
                path: "nb.ipynb".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = s.kernel.id;
        let weak = Arc::downgrade(&m.get(&id).unwrap());
        let mut c = m.connect_client(&id).unwrap();
        c.send(
            Channel::Shell,
            Message::new("execute_request", "sess", "u", &ExecuteRequest::new("die")),
        )
        .await
        .unwrap();
        let got = until_none(&mut c).await;
        let last = got.last().expect("status: dead").1.clone();
        assert_eq!(
            (last.msg_type(), last.content["execution_state"].as_str()),
            ("status", Some("dead"))
        );
        assert!(m.list().is_empty());
        assert_eq!(m.get(&id).unwrap_err().status, 404);
        assert!(sm.list().is_empty(), "its session goes too");
        drop(c);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(weak.upgrade().is_none(), "RunningKernel leaked");
    }

    #[tokio::test]
    async fn control_and_kernel_info_statuses_do_not_change_execution_state() {
        let m = manager(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let mut c = m.connect_client(&k.id).unwrap();
        c.send(
            Channel::Shell,
            Message::new(
                "execute_request",
                "sess",
                "u",
                &ExecuteRequest::new("sleep 2000"),
            ),
        )
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while m.model(&k.id).unwrap().execution_state != "busy" {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The fake answers kernel_info on control while a cell runs, with
        // busy and idle statuses of its own.
        let info = Message::new("kernel_info_request", "sess", "u", &serde_json::json!({}));
        let info_id = info.header.msg_id.clone();
        c.send(Channel::Control, info).await.unwrap();
        loop {
            let (ch, msg) = c.recv().await.unwrap();
            if ch == Channel::IoPub
                && msg.msg_type() == "status"
                && msg.parent_id() == Some(info_id.as_str())
                && msg.content["execution_state"] == "idle"
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(m.model(&k.id).unwrap().execution_state, "busy");
        m.shutdown(&k.id).await.unwrap();
    }

    fn statuses(got: &[(Channel, Message)]) -> Vec<String> {
        got.iter()
            .filter(|(ch, m)| *ch == Channel::IoPub && m.msg_type() == "status")
            .map(|(_, m)| m.content["execution_state"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn start_launches_again_when_a_kernel_dies_while_starting() {
        let (m, t) = scripted(None);
        t.script.lock().unwrap().extend([Launch::Die, Launch::Die]);
        let k = m.start(Some("fake"), None).await.unwrap();
        assert_eq!(k.execution_state, "idle");
        assert_eq!(m.list().len(), 1);
    }

    #[tokio::test]
    async fn start_gives_up_after_three_launches() {
        let (m, t) = scripted(None);
        t.script.lock().unwrap().extend([Launch::Die; 3]);
        assert!(m.start(Some("fake"), None).await.is_err());
        assert!(m.list().is_empty());
    }

    #[tokio::test]
    async fn failed_restart_reports_dead_and_removes_the_kernel() {
        let (m, t) = scripted(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let weak = Arc::downgrade(&m.get(&k.id).unwrap());
        let mut c = m.connect_client(&k.id).unwrap();
        t.script.lock().unwrap().push_back(Launch::Fail);
        assert!(m.restart(&k.id).await.is_err());
        let states = statuses(&until_none(&mut c).await);
        assert_eq!(
            states.first().map(String::as_str),
            Some("restarting"),
            "{states:?}"
        );
        assert_eq!(
            states.last().map(String::as_str),
            Some("dead"),
            "{states:?}"
        );
        assert!(m.list().is_empty());
        drop(c);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(weak.upgrade().is_none(), "RunningKernel leaked");
    }

    #[tokio::test]
    async fn restart_that_never_answers_reports_dead() {
        let (m, t) = scripted(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let mut c = m.connect_client(&k.id).unwrap();
        t.script.lock().unwrap().extend([Launch::Die; 3]);
        assert!(m.restart(&k.id).await.is_err());
        let states = statuses(&until_none(&mut c).await);
        assert_eq!(
            states.last().map(String::as_str),
            Some("dead"),
            "{states:?}"
        );
        assert!(m.list().is_empty());
    }

    #[tokio::test]
    async fn restart_racing_shutdown_leaves_nothing_running() {
        let m = manager(None);
        let k = m.start(Some("fake"), None).await.unwrap();
        let weak = Arc::downgrade(&m.get(&k.id).unwrap());
        // The shutdown waits for the restart, then stops the new kernel.
        let (restart, shutdown) = tokio::join!(m.restart(&k.id), m.shutdown(&k.id));
        assert!(restart.is_ok() && shutdown.is_ok());
        assert!(m.list().is_empty());

        // A restart that waited for a shutdown finds the kernel gone.
        let k = m.start(Some("fake"), None).await.unwrap();
        let weak2 = Arc::downgrade(&m.get(&k.id).unwrap());
        let (shutdown, restart) = tokio::join!(m.shutdown(&k.id), m.restart(&k.id));
        assert!(shutdown.is_ok());
        assert_eq!(restart.unwrap_err().status, 404);
        assert!(m.list().is_empty());

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(weak.upgrade().is_none() && weak2.upgrade().is_none());
    }
}
