//! The relay against real processes. `cat` stands in for a language
//! server: it echoes each framed message back, so what a client sends
//! comes back to every client through the server's stdout.
#![cfg(unix)]

use ganymede_lsp::{LanguageServerSpec, LspConfig, LspManager, VIRTUAL_DOCUMENTS_DIR};
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;

fn manager(root: &std::path::Path, specs: &[(&str, &[&str], bool)]) -> LspManager {
    let servers: BTreeMap<String, LanguageServerSpec> = specs
        .iter()
        .map(|(name, argv, on_disk)| {
            (
                name.to_string(),
                LanguageServerSpec {
                    argv: argv.iter().map(|s| s.to_string()).collect(),
                    languages: vec!["fake".into()],
                    requires_documents_on_disk: *on_disk,
                    ..Default::default()
                },
            )
        })
        .collect();
    LspManager::new(LspConfig {
        servers,
        root_dir: root.into(),
    })
}

async fn next(c: &mut ganymede_lsp::LspClient) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(10), c.recv())
        .await
        .expect("timed out waiting for the server")
        .map(|m| m.to_string())
}

#[tokio::test]
async fn shared_process_broadcasts_and_stops_with_last_client() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("echo", &["cat"], false)]);
    let mut a = m.connect("echo").unwrap();
    let mut b = m.connect("echo").unwrap();
    let st = m.status();
    assert_eq!(st["sessions"]["echo"]["status"], "started");
    assert_eq!(st["sessions"]["echo"]["handler_count"], 2);

    let msg = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"x":"é"}}"#;
    a.send(msg.into()).await;
    assert_eq!(next(&mut a).await.as_deref(), Some(msg));
    assert_eq!(next(&mut b).await.as_deref(), Some(msg));
    let st = m.status();
    assert!(st["sessions"]["echo"]["last_handler_message_at"].is_string());
    assert!(st["sessions"]["echo"]["last_server_message_at"].is_string());

    drop(a);
    assert_eq!(m.status()["sessions"]["echo"]["handler_count"], 1);
    drop(b);
    assert_eq!(m.status()["sessions"]["echo"]["status"], "stopped");

    // The next client starts a new process.
    let mut c = m.connect("echo").unwrap();
    c.send("{}".into()).await;
    assert_eq!(next(&mut c).await.as_deref(), Some("{}"));
}

#[tokio::test]
async fn exited_server_closes_clients_and_restarts_on_next_connect() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("quits", &["sh", "-c", "exit 0"], false)]);
    let mut a = m.connect("quits").unwrap();
    assert_eq!(next(&mut a).await, None);
    assert_eq!(m.status()["sessions"]["quits"]["status"], "stopped");
    let mut b = m.connect("quits").unwrap();
    assert_eq!(next(&mut b).await, None);
}

#[tokio::test]
async fn unknown_and_unstartable_servers() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(
        root.path(),
        &[("missing", &["/nonexistent/language-server"], false)],
    );
    assert_eq!(m.connect("nope").err().unwrap().status(), 404);
    assert_eq!(m.connect("missing").err().unwrap().status(), 500);
    assert_eq!(m.status()["sessions"]["missing"]["status"], "not_started");
}

#[tokio::test]
async fn env_reaches_the_server() {
    let root = tempfile::tempdir().unwrap();
    // Echoes a message whose body is the expanded variable.
    let script = r#"printf 'Content-Length: %s\r\n\r\n%s' "${#GANYMEDE_LSP_TEST}" "$GANYMEDE_LSP_TEST"; cat >/dev/null"#;
    let servers = BTreeMap::from([(
        "env".to_string(),
        LanguageServerSpec {
            argv: vec!["sh".into(), "-c".into(), script.into()],
            env: BTreeMap::from([("GANYMEDE_LSP_TEST".to_string(), "${HOME}|$$".to_string())]),
            ..Default::default()
        },
    )]);
    let m = LspManager::new(LspConfig {
        servers,
        root_dir: root.path().into(),
    });
    let mut c = m.connect("env").unwrap();
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(next(&mut c).await, Some(format!("{home}|$")));
}

#[tokio::test]
async fn virtual_documents_are_written_before_relaying() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(
        root.path(),
        &[("disk", &["cat"], true), ("memory", &["cat"], false)],
    );
    let vd = ganymede_lsp::virtual_documents_uri(root.path());
    let open = |uri: &str, text: &str| {
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": uri, "languageId": "fake", "version": 0, "text": text}}})
        .to_string()
    };

    let mut disk = m.connect("disk").unwrap();
    let msg = open(&format!("{vd}/nb.ipynb"), "a = 1\n");
    disk.send(msg.clone()).await;
    // Relayed unchanged, and on disk by the time the server sees it.
    assert_eq!(next(&mut disk).await, Some(msg));
    let file = root.path().join(VIRTUAL_DOCUMENTS_DIR).join("nb.ipynb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a = 1\n");

    let mut memory = m.connect("memory").unwrap();
    let msg = open(&format!("{vd}/other.ipynb"), "b = 2\n");
    memory.send(msg.clone()).await;
    assert_eq!(next(&mut memory).await, Some(msg));
    assert!(!root
        .path()
        .join(VIRTUAL_DOCUMENTS_DIR)
        .join("other.ipynb")
        .exists());
}

#[tokio::test]
async fn stopping_a_server_ends_what_it_started() {
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("worker.pid");
    // A wrapper with a worker, as npm shims and typescript-language-server
    // (which runs tsserver) are.
    let script = format!("sleep 300 & echo $! > '{}'; cat", pidfile.display());
    let m = manager(root.path(), &[("wrapped", &["sh", "-c", &script], false)]);
    let mut c = m.connect("wrapped").unwrap();
    c.send("{}".into()).await;
    assert_eq!(next(&mut c).await.as_deref(), Some("{}"));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(pid));
    drop(c);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "worker {pid} outlived its server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
