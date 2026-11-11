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

/// The file ids for `root`, in a database in it.
fn file_ids(root: &std::path::Path) -> ganymede_contents::FileIds {
    let root = root.canonicalize().unwrap();
    let db = root.join(".ids.db");
    ganymede_contents::FileIds::open(&db, &root)
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
        // File ids in a database, as the server keeps them, so a test can
        // store a grant from outside, as `ganymede grant` does.
        collab: Arc::new(ganymede_server::collab::Collab::new(
            file_ids(root),
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
        ..Default::default()
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
        [
            "notebooks.list",
            "notebooks.read",
            "notebooks.search",
            "notebooks.results",
            "self.add",
            "self.edit",
            "self.delete"
        ]
    );
    // A kernel with no notebook has nothing of its own to edit.
    let none = api
        .hello(&Caller {
            kernel_id: "no-such-kernel".into(),
        })
        .await;
    assert_eq!(none.notebook, None);
    assert_eq!(
        none.methods,
        [
            "notebooks.list",
            "notebooks.read",
            "notebooks.search",
            "notebooks.results"
        ]
    );
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
        .call(&caller, "notebooks.edit", json!({}))
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

#[tokio::test]
async fn results_come_from_a_notebooks_outputs_under_the_read_grant() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let result = |name: &str, value: Value| {
        json!({"output_type": "display_data", "metadata": {},
               "data": {"application/vnd.ganymede.result+json": {"name": name, "value": value},
                        "text/plain": name.to_string()}})
    };
    let nb = json!({"cells": [
        {"cell_type": "code", "id": "a", "metadata": {}, "source": "", "execution_count": 1,
         "outputs": [result("weekday_drop", json!(-0.19)), result("days", json!(3))]},
        {"cell_type": "code", "id": "b", "metadata": {}, "source": "", "execution_count": 2,
         "outputs": [result("days", json!(5))]}],
        "metadata": {}, "nbformat": 4, "nbformat_minor": 5});
    for path in ["journal/r.ipynb", "journal-private/r.ipynb"] {
        std::fs::write(dir.path().join(path), nb.to_string()).unwrap();
    }
    let (api, caller) = setup(dir.path(), "index.ipynb", &["journal"]).await;
    let results = api
        .call(
            &caller,
            "notebooks.results",
            json!({"path": "journal/r.ipynb"}),
        )
        .await
        .unwrap();
    // The last output of a name wins.
    assert_eq!(results, json!({"weekday_drop": -0.19, "days": 5}));
    // A notebook without results has none.
    assert_eq!(
        api.call(&caller, "notebooks.results", json!({"path": "index.ipynb"}))
            .await
            .unwrap(),
        json!({})
    );
    let err = api
        .call(
            &caller,
            "notebooks.results",
            json!({"path": "journal-private/r.ipynb"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
}

#[tokio::test]
async fn a_notebooks_own_grant_replaces_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[""]).await;
    let read = |path: &'static str| {
        let (api, caller) = (&api, &caller);
        async move {
            api.call(caller, "notebooks.read", json!({ "path": path }))
                .await
                .map(|_| ())
                .map_err(|e| e.code)
        }
    };
    assert_eq!(read("journal-private/p.ipynb").await, Ok(()));

    // As `ganymede grant index.ipynb read=journal edit_self=false` stores it.
    let ids = file_ids(dir.path());
    let (id, _) = ids.index("index.ipynb");
    ids.set_grant(&id, Some(r#"{"read":["journal"],"edit_self":false}"#))
        .unwrap();
    assert_eq!(read("journal-private/p.ipynb").await, Err("denied"));
    assert_eq!(read("journal/a.ipynb").await, Ok(()));
    assert_eq!(read("index.ipynb").await, Ok(()));
    assert!(!api
        .hello(&caller)
        .await
        .methods
        .iter()
        .any(|m| m.starts_with("self.")));
    let err = api
        .call(&caller, "self.add", json!({"source": "x"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert!(err.message.contains("edit_self"), "{}", err.message);

    // The grant follows a rename made through the server.
    ids.moved("index.ipynb", "home.ipynb");
    std::fs::rename(
        dir.path().join("index.ipynb"),
        dir.path().join("home.ipynb"),
    )
    .unwrap();
    let (api, caller) = setup(dir.path(), "home.ipynb", &[""]).await;
    let err = api
        .call(
            &caller,
            "notebooks.read",
            json!({"path": "journal-private/p.ipynb"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");

    // Removed, the defaults apply again.
    ids.set_grant(&id, None).unwrap();
    assert!(api
        .call(
            &caller,
            "notebooks.read",
            json!({"path": "journal-private/p.ipynb"})
        )
        .await
        .is_ok());
}

/// The kernels running a `notebooks.run`, by the launch variable the
/// server gave them.
fn run_kernels(api_state: &AppState) -> Vec<String> {
    api_state
        .kernels
        .list()
        .into_iter()
        .filter(|k| {
            api_state
                .kernels
                .get(&k.id)
                .is_ok_and(|k| k.launch_var("GANYMEDE_HOST_RUN").is_some())
        })
        .map(|k| k.id)
        .collect()
}

#[tokio::test]
async fn a_notebook_runs_another_under_its_run_grant() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let code = |id: &str, src: &str| json!({"cell_type": "code", "execution_count": null, "id": id, "metadata": {}, "outputs": [], "source": src});
    std::fs::create_dir(dir.path().join("reports")).unwrap();
    let write = |name: &str, cells: Vec<Value>| {
        let nb = json!({"cells": cells, "nbformat": 4, "nbformat_minor": 5,
            "metadata": {"kernelspec": {"name": "fake", "display_name": "Fake"}}});
        std::fs::write(dir.path().join(name), nb.to_string()).unwrap();
    };
    write("reports/r.ipynb", vec![code("a", "result total 42")]);
    write(
        "reports/slow.ipynb",
        vec![code("a", "result early true"), code("b", "sleep 3000")],
    );
    let state = state(dir.path());
    let session = state
        .sessions
        .create(SessionCreate {
            path: "index.ipynb".into(),
            name: None,
            type_: Some("notebook".into()),
            kernel: Default::default(),
        })
        .await
        .unwrap();
    let caller = Caller {
        kernel_id: session.kernel.id.clone(),
    };
    let api = Arc::new(HostApi::new(
        state.clone(),
        &HostConfig {
            run: vec!["reports".into()],
            ..Default::default()
        },
    ));
    assert!(api
        .hello(&caller)
        .await
        .methods
        .contains(&"notebooks.run".to_string()));

    let ran = api
        .call(&caller, "notebooks.run", json!({"path": "reports/r.ipynb"}))
        .await
        .unwrap();
    assert_eq!(
        ran,
        json!({"path": "reports/r.ipynb", "status": "ok", "results": {"total": 42}})
    );
    let err = api
        .call(&caller, "notebooks.run", json!({"path": "journal/a.ipynb"}))
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert!(err.message.contains("run grant"), "{}", err.message);

    // The run's kernel gets the intersection of the two grants: the
    // called notebook may read only journal/, so its run may too.
    let ids = file_ids(dir.path());
    let (id, _) = ids.index("reports/slow.ipynb");
    ids.set_grant(&id, Some(r#"{"read":["journal"]}"#)).unwrap();
    let running = tokio::spawn({
        let (api, caller) = (api.clone(), caller.clone());
        async move {
            api.call(
                &caller,
                "notebooks.run",
                json!({"path": "reports/slow.ipynb"}),
            )
            .await
        }
    });
    let run_kernel = loop {
        if let Some(k) = run_kernels(&state).pop() {
            break Caller { kernel_id: k };
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let hello = api.hello(&run_kernel).await;
    assert_eq!(hello.notebook.as_deref(), Some("reports/slow.ipynb"));
    assert!(
        !hello.methods.iter().any(|m| m.starts_with("self.")),
        "a run writes nothing: {:?}",
        hello.methods
    );
    let read = |path: &'static str| {
        let (api, k) = (&api, &run_kernel);
        async move {
            api.call(k, "notebooks.read", json!({ "path": path }))
                .await
                .map(|_| ())
                .map_err(|e| e.code)
        }
    };
    assert_eq!(read("journal/a.ipynb").await, Ok(()));
    assert_eq!(read("journal-private/p.ipynb").await, Err("denied"));

    // A run may run another, one deeper, under the narrowed run grant.
    let nested = api
        .call(
            &run_kernel,
            "notebooks.run",
            json!({"path": "reports/r.ipynb"}),
        )
        .await
        .unwrap();
    assert_eq!(nested["results"], json!({"total": 42}));

    // Cancelling the call (the comm aborts it) shuts the run's kernel
    // down.
    running.abort();
    let _ = running.await;
    for _ in 0..200 {
        if run_kernels(&state).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(run_kernels(&state).is_empty());
}

#[tokio::test]
async fn a_kernel_has_two_runs_in_flight_at_most() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let nb = json!({"cells": [{"cell_type": "code", "execution_count": null, "id": "a",
                               "metadata": {}, "outputs": [], "source": "sleep 400"}],
                    "nbformat": 4, "nbformat_minor": 5,
                    "metadata": {"kernelspec": {"name": "fake", "display_name": "Fake"}}});
    std::fs::write(dir.path().join("slow.ipynb"), nb.to_string()).unwrap();
    let state = state(dir.path());
    let session = state
        .sessions
        .create(SessionCreate {
            path: "index.ipynb".into(),
            name: None,
            type_: Some("notebook".into()),
            kernel: Default::default(),
        })
        .await
        .unwrap();
    let caller = Caller {
        kernel_id: session.kernel.id.clone(),
    };
    let api = Arc::new(HostApi::new(
        state.clone(),
        &HostConfig {
            run: vec![String::new()],
            ..Default::default()
        },
    ));
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let (api, caller) = (api.clone(), caller.clone());
            tokio::spawn(async move {
                api.call(&caller, "notebooks.run", json!({"path": "slow.ipynb"}))
                    .await
            })
        })
        .collect();
    let mut most = 0;
    while !calls.iter().all(|c| c.is_finished()) {
        most = most.max(run_kernels(&state).len());
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    for c in calls {
        assert_eq!(c.await.unwrap().unwrap()["status"], "ok");
    }
    assert_eq!(most, 2);
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

#[cfg(unix)]
#[tokio::test]
async fn a_granted_folder_that_is_a_symlink_still_covers_its_notebooks() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    std::fs::create_dir(dir.path().join("archive")).unwrap();
    std::fs::rename(
        dir.path().join("journal"),
        dir.path().join("archive/journal"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("archive/journal"),
        dir.path().join("journal"),
    )
    .unwrap();
    let (api, caller) = setup(dir.path(), "index.ipynb", &["journal"]).await;
    assert!(api
        .call(
            &caller,
            "notebooks.read",
            json!({"path": "journal/a.ipynb"})
        )
        .await
        .is_ok());
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
    let api = HostApi::new(
        state.clone(),
        &HostConfig {
            read: vec![],
            ..Default::default()
        },
    );
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

/// The cells of `path` as `notebooks.read` returns them: `(id, source, hash)`.
async fn cells(api: &HostApi, caller: &Caller, path: &str) -> Vec<(String, String, String)> {
    let read = api
        .call(caller, "notebooks.read", json!({ "path": path }))
        .await
        .unwrap();
    read["notebook"]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let id = c["id"].as_str().unwrap().to_string();
            let source = match &c["source"] {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => String::new(),
            };
            let hash = read["hashes"][&id].as_str().unwrap().to_string();
            (id, source, hash)
        })
        .collect()
}

#[tokio::test]
async fn a_notebook_adds_edits_and_deletes_its_own_cells() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let (api, caller) = setup(dir.path(), "index.ipynb", &[]).await;
    let first = cells(&api, &caller, "index.ipynb").await[0].clone();

    let added = api
        .call(
            &caller,
            "self.add",
            json!({"cell_type": "markdown", "source": "| a |", "after": first.0}),
        )
        .await
        .unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    let now = cells(&api, &caller, "index.ipynb").await;
    assert_eq!(now.len(), 2);
    assert_eq!(
        (now[1].0.as_str(), now[1].1.as_str()),
        (id.as_str(), "| a |")
    );

    // An edit names the hash it read.
    let hash = now[1].2.clone();
    api.call(
        &caller,
        "self.edit",
        json!({"id": id, "source": "| b |", "source_hash": hash}),
    )
    .await
    .unwrap();
    assert_eq!(cells(&api, &caller, "index.ipynb").await[1].1, "| b |");
    // The old hash is stale now.
    let err = api
        .call(
            &caller,
            "self.edit",
            json!({"id": id, "source": "| c |", "source_hash": hash}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "failed");
    assert!(
        err.message.starts_with("source_mismatch"),
        "{}",
        err.message
    );
    let err = api
        .call(
            &caller,
            "self.delete",
            json!({"id": id, "source_hash": hash}),
        )
        .await
        .unwrap_err();
    assert!(
        err.message.starts_with("source_mismatch"),
        "{}",
        err.message
    );

    let hash = cells(&api, &caller, "index.ipynb").await[1].2.clone();
    api.call(
        &caller,
        "self.delete",
        json!({"id": id, "source_hash": hash}),
    )
    .await
    .unwrap();
    assert_eq!(cells(&api, &caller, "index.ipynb").await.len(), 1);

    let err = api
        .call(
            &caller,
            "self.delete",
            json!({"id": "nope", "source_hash": "0"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.message, "no cell nope");

    // Bad params fail without touching the notebook.
    for params in [
        json!({"cell_type": 1, "source": "x = 1"}),
        json!({"cell_type": "heading", "source": "x"}),
        json!({"source": "x", "after": "nope"}),
    ] {
        let err = api.call(&caller, "self.add", params).await.unwrap_err();
        assert_eq!(err.code, "failed", "{}", err.message);
    }
    assert_eq!(cells(&api, &caller, "index.ipynb").await.len(), 1);

    // A kernel with no notebook has nothing to edit.
    let err = api
        .call(
            &Caller {
                kernel_id: "no-such-kernel".into(),
            },
            "self.add",
            json!({"source": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "unavailable");
}

#[tokio::test]
async fn edit_self_off_takes_the_self_calls_away() {
    let dir = tempfile::tempdir().unwrap();
    notebooks(dir.path());
    let state = state(dir.path());
    let session = state
        .sessions
        .create(SessionCreate {
            path: "index.ipynb".into(),
            name: None,
            type_: Some("notebook".into()),
            kernel: Default::default(),
        })
        .await
        .unwrap();
    let caller = Caller {
        kernel_id: session.kernel.id.clone(),
    };
    let api = HostApi::new(
        state,
        &HostConfig {
            edit_self: false,
            ..Default::default()
        },
    );
    assert!(!api
        .hello(&caller)
        .await
        .methods
        .iter()
        .any(|m| m.starts_with("self.")));
    let err = api
        .call(
            &caller,
            "self.add",
            json!({"cell_type": "markdown", "source": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "denied");
    assert!(err.message.contains("[host] edit_self"), "{}", err.message);
}
