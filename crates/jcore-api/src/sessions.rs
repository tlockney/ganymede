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

    pub fn list(&self) -> Vec<SessionModel> {
        let sessions = self.sessions.read().unwrap();
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
    use jcore_kernel_host::protocol::{content::ExecuteRequest, Channel, ConnectionInfo, Message};
    use jcore_kernel_host::testing::{FakeKernel, ReplyOrder};
    use jcore_kernel_host::{
        attach, in_process_pair, KernelTransport, Kernelspec, LaunchOptions, LaunchedKernel,
        Provisioner,
    };
    use std::time::Duration;

    struct FakeProvisioner;
    #[async_trait]
    impl Provisioner for FakeProvisioner {
        async fn launch(
            &self,
            _spec: &Kernelspec,
            _opts: LaunchOptions,
        ) -> jcore_kernel_host::Result<LaunchedKernel> {
            Ok(attach(ConnectionInfo::new_tcp(
                "127.0.0.1",
                [1, 2, 3, 4, 5],
            )))
        }
    }

    struct FakeTransports;
    #[async_trait]
    impl TransportFactory for FakeTransports {
        async fn connect(
            &self,
            _launched: &LaunchedKernel,
        ) -> jcore_kernel_host::Result<KernelTransport> {
            let (host, side) = in_process_pair(256);
            FakeKernel::spawn(side, ReplyOrder::ReplyThenIdle);
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
            interrupt_mode: jcore_kernel_host::InterruptMode::Message,
            env: Default::default(),
            metadata: serde_json::json!({"capabilities": {"jit": false}})
                .as_object()
                .unwrap()
                .clone(),
            extra: Default::default(),
        }
    }

    pub(crate) fn manager(cull: Option<Duration>) -> Arc<KernelManager> {
        Arc::new(KernelManager::new(
            Arc::new(StaticKernelspecs(vec![
                fake_spec("fake"),
                fake_spec("other"),
            ])),
            Arc::new(FakeProvisioner),
            Arc::new(FakeTransports),
            KernelManagerConfig {
                startup_timeout: Duration::from_secs(5),
                cull_idle: cull,
                ..Default::default()
            },
        ))
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
}
