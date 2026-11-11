//! The host API's server side (`docs/host-api.md`): what a kernel's
//! notebook calls read, and which paths a grant lets through.

use async_trait::async_trait;
use ganymede_api::{
    KernelManager, KernelManagerConfig, SessionCreate, SessionManager, StaticKernelspecs,
    TransportFactory,
};
use ganymede_contents::{FsContents, FsOptions};
use ganymede_host::{Caller, Responder};
use ganymede_kernel_host::protocol::ConnectionInfo;
use ganymede_kernel_host::testing::{FakeKernel, ReplyOrder};
use ganymede_kernel_host::{
    attach, in_process_pair, KernelTransport, Kernelspec, LaunchOptions, LaunchedKernel,
    Provisioner,
};
use ganymede_server::host::{HostApi, HostConfig};
use ganymede_server::AppState;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const TOKEN: &str = "test-token";

struct FakeProvisioner;
#[async_trait]
impl Provisioner for FakeProvisioner {
    async fn launch(
        &self,
        _: &Kernelspec,
        _: LaunchOptions,
    ) -> ganymede_kernel_host::Result<LaunchedKernel> {
        Ok(attach(ConnectionInfo::new_tcp(
            "127.0.0.1",
            [1, 2, 3, 4, 5],
        )))
    }
}
struct FakeTransports;
#[async_trait]
impl TransportFactory for FakeTransports {
    async fn connect(&self, _: &LaunchedKernel) -> ganymede_kernel_host::Result<KernelTransport> {
        let (host, side) = in_process_pair(256);
        FakeKernel::spawn(side, ReplyOrder::IdleThenReply);
        Ok(host)
    }
}

fn state(root: &std::path::Path) -> AppState {
    state_with(root, Arc::new(FakeProvisioner), Arc::new(FakeTransports))
}

fn state_with(
    root: &std::path::Path,
    provisioner: Arc<dyn Provisioner>,
    transports: Arc<dyn TransportFactory>,
) -> AppState {
    let spec = Kernelspec {
        name: "fake".into(),
        resource_dir: root.into(),
        argv: vec!["fake".into()],
        display_name: "Fake".into(),
        language: "fake".into(),
        interrupt_mode: ganymede_kernel_host::InterruptMode::Message,
        env: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    };
    let kernels = Arc::new(KernelManager::new(
        Arc::new(StaticKernelspecs(vec![spec])),
        provisioner,
        transports,
        KernelManagerConfig {
            startup_timeout: Duration::from_secs(5),
            root_dir: root.into(),
            ..Default::default()
        },
    ));
    AppState {
        sessions: Arc::new(SessionManager::new(kernels.clone())),
        kernels,
        contents: Arc::new(FsContents::new(root, FsOptions::default()).unwrap()),
        token: Some(TOKEN.into()),
        cookie_secret: Arc::from(&b"test-secret"[..]),
        allow_origins: vec![],
        allow_credentials: false,
        allow_remote_access: false,
        local_hostnames: vec!["localhost".into()],
        trust_xheaders: false,
        max_body_size: 512 * 1024 * 1024,
        base_url: "/".into(),
        config_dir: root.join(".jupyter-config"),
        lab: None,
        static_dir: None,
        static_headers: Arc::new([]),
        frame_ancestors: ganymede_server::frame_ancestors_directive("'self'").unwrap(),
        collab: Arc::new(ganymede_server::collab::Collab::new(
            ganymede_contents::FileIds::in_memory(),
            root.canonicalize().ok(),
        )),
        terminals: None,
        lsp: None,
        mcp: None,
        metrics: Default::default(),
    }
}

fn notebooks(root: &std::path::Path) {
    let nb = |cells: Value| {
        json!({"cells": cells, "metadata": {"kernelspec": {"name": "fake", "display_name": "Fake"}},
               "nbformat": 4, "nbformat_minor": 5})
        .to_string()
    };
    std::fs::create_dir_all(root.join("journal")).unwrap();
    std::fs::create_dir_all(root.join("journal-private")).unwrap();
    std::fs::write(
        root.join("journal/a.ipynb"),
        nb(json!([
            {"cell_type": "markdown", "id": "t", "metadata": {}, "source": "# Fremont Bridge"},
            {"cell_type": "markdown", "id": "f", "metadata": {},
             "source": "Workday mornings fell 19% against 2019."}])),
    )
    .unwrap();
    std::fs::write(
        root.join("journal-private/p.ipynb"),
        nb(json!([{"cell_type": "markdown", "id": "p", "metadata": {}, "source": "workday private"}])),
    )
    .unwrap();
    std::fs::write(
        root.join("index.ipynb"),
        nb(json!([{"cell_type": "markdown", "id": "i", "metadata": {}, "source": "# Index\nworkday"}])),
    )
    .unwrap();
}

/// A kernel started for `path`'s session, and a host API with `read`.
async fn setup(root: &std::path::Path, path: &str, read: &[&str]) -> (HostApi, Caller) {
    let state = state(root);
    let session = state
        .sessions
        .create(SessionCreate {
            path: path.into(),
            name: Some(path.into()),
            type_: Some("notebook".into()),
            kernel: Default::default(),
        })
        .await
        .unwrap();
    let config = HostConfig {
        read: read.iter().map(|s| s.to_string()).collect(),
    };
    let caller = Caller {
        kernel_id: session.kernel.id.clone(),
    };
    (HostApi::new(state, &config), caller)
}

#[tokio::test]
async fn hello_names_the_notebook_and_methods() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[""]).await;
    let hello = api.hello(&caller).await;
    assert_eq!(hello.notebook.as_deref(), Some("index.ipynb"));
    assert_eq!(
        hello.methods,
        ["notebooks.list", "notebooks.read", "notebooks.search"]
    );
    let none = api
        .hello(&Caller {
            kernel_id: "no-such-kernel".into(),
        })
        .await;
    assert_eq!(none.notebook, None);
}

#[tokio::test]
async fn list_read_and_search_return_data() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[""]).await;

    let list = api
        .call(&caller, "notebooks.list", json!({"dir": "journal"}))
        .await
        .unwrap();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["path"], "journal/a.ipynb");
    assert_eq!(list[0]["title"], "Fremont Bridge");
    assert_eq!(list[0]["kernel"], "fake");

    let read = api
        .call(
            &caller,
            "notebooks.read",
            json!({"path": "journal/a.ipynb"}),
        )
        .await
        .unwrap();
    assert_eq!(read["notebook"]["cells"][1]["id"], "f");
    let source = "Workday mornings fell 19% against 2019.";
    assert_eq!(
        read["hashes"]["f"],
        ganymede_documents::murmur2(source.as_bytes(), 0).to_string()
    );

    let found = api
        .call(
            &caller,
            "notebooks.search",
            json!({"query": "workday", "dir": "journal"}),
        )
        .await
        .unwrap();
    assert_eq!(found["total"], 1);
    assert_eq!(found["hits"][0]["path"], "journal/a.ipynb");
    assert_eq!(found["hits"][0]["id"], "f");
    assert_eq!(found["hits"][0]["title"], "Fremont Bridge");

    let err = api
        .call(&caller, "notebooks.run", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "unavailable");
    let err = api
        .call(&caller, "notebooks.search", json!({"query": " "}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "failed");
}

#[tokio::test]
async fn a_read_grant_matches_whole_segments() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &["journal"]).await;
    let read = |path: &'static str| api.call(&caller, "notebooks.read", json!({ "path": path }));
    assert!(read("journal/a.ipynb").await.is_ok());
    assert_eq!(
        read("journal-private/p.ipynb").await.unwrap_err().code,
        "denied"
    );
    assert_eq!(
        read("journal/../journal-private/p.ipynb")
            .await
            .unwrap_err()
            .code,
        "denied"
    );
    // Its own notebook, outside the grant, is readable.
    assert!(read("index.ipynb").await.is_ok());
    // A search from the root covers only what the grant reaches.
    let found = api
        .call(&caller, "notebooks.search", json!({"query": "workday"}))
        .await
        .unwrap();
    let paths: Vec<&str> = found["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["index.ipynb", "journal/a.ipynb"]);
    // Listing the root shows directories, and only the readable notebooks.
    let list = api
        .call(&caller, "notebooks.list", json!({}))
        .await
        .unwrap();
    let paths: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"index.ipynb") && paths.contains(&"journal"),
        "{paths:?}"
    );
    // Not the directories the grant doesn't reach.
    assert!(!paths.contains(&"journal-private"), "{paths:?}");
    // A directory outside the grant can't be listed.
    let err = api
        .call(&caller, "notebooks.list", json!({"dir": "journal-private"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

#[tokio::test]
async fn no_read_grant_leaves_only_its_own_notebook() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[]).await;
    assert!(api
        .call(&caller, "notebooks.read", json!({"path": "index.ipynb"}))
        .await
        .is_ok());
    assert_eq!(
        api.call(
            &caller,
            "notebooks.read",
            json!({"path": "journal/a.ipynb"})
        )
        .await
        .unwrap_err()
        .code,
        "denied"
    );
}

fn paths_of(v: &Value, key: &str) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e[key].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_empty_grant_still_searches_and_lists_its_own_notebook() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[]).await;
    let found = api
        .call(&caller, "notebooks.search", json!({"query": "workday"}))
        .await
        .unwrap();
    assert_eq!(paths_of(&found["hits"], "path"), ["index.ipynb"]);
    let list = api
        .call(&caller, "notebooks.list", json!({}))
        .await
        .unwrap();
    assert_eq!(paths_of(&list, "path"), ["index.ipynb"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_in_a_granted_folder_leads_nowhere_else() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    std::os::unix::fs::symlink(
        dir.path().join("journal-private"),
        dir.path().join("journal/link"),
    )
    .unwrap();
    let (api, caller) = setup(dir.path(), "index.ipynb", &["journal"]).await;
    let err = api
        .call(
            &caller,
            "notebooks.read",
            json!({"path": "journal/link/p.ipynb"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
    let found = api
        .call(
            &caller,
            "notebooks.search",
            json!({"query": "private", "dir": "journal"}),
        )
        .await
        .unwrap();
    assert_eq!(found["hits"], json!([]), "{found}");
}

#[tokio::test]
async fn a_notebook_that_joins_the_kernel_does_not_take_its_place() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let state = state(dir.path());
    let create = |path: String, kernel: Option<String>| SessionCreate {
        path,
        name: None,
        type_: Some("notebook".into()),
        kernel: ganymede_api::sessions::KernelRef {
            id: kernel,
            name: None,
        },
    };
    let first = state
        .sessions
        .create(create("index.ipynb".into(), None))
        .await
        .unwrap();
    let caller = Caller {
        kernel_id: first.kernel.id.clone(),
    };
    let api = HostApi::new(state.clone(), &HostConfig { read: vec![] });
    assert_eq!(
        api.hello(&caller).await.notebook.as_deref(),
        Some("index.ipynb")
    );
    // Several notebooks join, so one sorts before the first by id.
    for i in 0..8 {
        state
            .sessions
            .create(create(
                format!("j{i}.ipynb"),
                Some(caller.kernel_id.clone()),
            ))
            .await
            .unwrap();
    }
    assert_eq!(
        api.hello(&caller).await.notebook.as_deref(),
        Some("index.ipynb")
    );
}
