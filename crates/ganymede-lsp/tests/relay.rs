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

    // A notification goes to every client, unchanged.
    let msg = r#"{"jsonrpc":"2.0","method":"initialized","params":{"x":"é"}}"#;
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

/// Two tabs on one notebook: the server gets the document once, each
/// edit once, and one close when both tabs have left (#262).
#[tokio::test]
async fn two_clients_on_one_document_apply_each_edit_once() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("disk", &["cat"], true)]);
    let vd = ganymede_lsp::virtual_documents_uri(root.path());
    let uri = format!("{vd}/nb.ipynb");
    let open = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": {"uri": uri, "languageId": "fake", "version": 0, "text": "a = 1\n"}}})
    .to_string();
    let edit = |version: i64| {
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"range": {"start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 0}}, "text": "# x\n"}]}})
        .to_string()
    };
    let ping = r#"{"jsonrpc":"2.0","method":"ping"}"#;

    let mut a = m.connect("disk").unwrap();
    let b = m.connect("disk").unwrap();
    // Keeps the server running, and reads what it gets after `a` leaves.
    let mut c = m.connect("disk").unwrap();
    a.send(open.clone()).await;
    b.send(open.clone()).await;
    a.send(edit(1)).await;
    b.send(edit(1)).await;
    b.send(ping.into()).await;
    // What the server got, in order, up to the marker.
    let mut got = Vec::new();
    loop {
        let m = next(&mut a).await.unwrap();
        if m == ping {
            break;
        }
        got.push(serde_json::from_str::<serde_json::Value>(&m).unwrap());
    }
    let methods: Vec<_> = got.iter().map(|m| m["method"].clone()).collect();
    assert_eq!(
        methods,
        ["textDocument/didOpen", "textDocument/didChange"],
        "{got:?}"
    );
    let file = root.path().join(VIRTUAL_DOCUMENTS_DIR).join("nb.ipynb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# x\na = 1\n");

    // Both tabs leave: one didClose, after the second.
    drop(b);
    drop(a);
    let mut closes = 0;
    loop {
        let m = next(&mut c).await.unwrap();
        if m.contains("textDocument/didClose") {
            closes += 1;
            break;
        }
    }
    // Each tab's cleanup sends what it sends at once, under the documents
    // lock, and the close is the last of them, from whichever ran second.
    // So both have sent everything by now, and a marker sent now comes
    // after it all.
    c.send(ping.into()).await;
    loop {
        let m = next(&mut c).await.unwrap();
        if m == ping {
            break;
        }
        if m.contains("textDocument/didClose") {
            closes += 1;
        }
    }
    assert_eq!(closes, 1);
}

/// Tabs that don't share edits: a hover from the tab whose text the
/// server doesn't have reaches the server after that tab's full text,
/// and the virtual document on disk follows it.
#[tokio::test]
async fn a_request_from_a_tab_behind_follows_its_text() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("disk", &["cat"], true)]);
    let vd = ganymede_lsp::virtual_documents_uri(root.path());
    let uri = format!("{vd}/nb.ipynb");
    let open = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": {"uri": uri, "languageId": "fake", "version": 0, "text": "a\n"}}})
    .to_string();
    let edit = json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
        "textDocument": {"uri": uri, "version": 1},
        "contentChanges": [{"range": {"start": {"line": 0, "character": 1},
            "end": {"line": 0, "character": 1}}, "text": "b"}]}})
    .to_string();
    let hover = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover", "params": {
        "textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}})
    .to_string();
    let ping = r#"{"jsonrpc":"2.0","method":"ping"}"#;

    let mut a = m.connect("disk").unwrap();
    let b = m.connect("disk").unwrap();
    a.send(open.clone()).await;
    b.send(open).await;
    a.send(edit).await;
    let file = root.path().join(VIRTUAL_DOCUMENTS_DIR).join("nb.ipynb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "ab\n");
    b.send(hover).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\n");
    b.send(ping.into()).await;
    let mut got = Vec::new();
    loop {
        let m = next(&mut a).await.unwrap();
        if m == ping {
            break;
        }
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        got.push((
            v["method"].clone(),
            v["params"]["contentChanges"][0]["text"].clone(),
        ));
    }
    assert_eq!(
        got,
        [
            (json!("textDocument/didOpen"), json!(null)),
            (json!("textDocument/didChange"), json!("b")),
            (json!("textDocument/didChange"), json!("a\n")),
            (json!("textDocument/hover"), json!(null)),
        ]
    );
}

/// A `completionItem/resolve` names no document: it reaches the server
/// after the text of the document the tab last asked about, even when
/// the other tab changed it in between (#334).
#[tokio::test]
async fn a_resolve_from_a_tab_behind_follows_its_text() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("disk", &["cat"], true)]);
    let vd = ganymede_lsp::virtual_documents_uri(root.path());
    let uri = format!("{vd}/nb.ipynb");
    let open = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": {"uri": uri, "languageId": "fake", "version": 0, "text": "a\n"}}})
    .to_string();
    let edit = |version: i64, at: i64, text: &str| {
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"range": {"start": {"line": 0, "character": at},
                "end": {"line": 0, "character": at}}, "text": text}]}})
        .to_string()
    };
    let complete = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/completion",
        "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}})
    .to_string();
    let resolve = json!({"jsonrpc": "2.0", "id": 2, "method": "completionItem/resolve",
        "params": {"label": "abc"}})
    .to_string();
    let ping = r#"{"jsonrpc":"2.0","method":"ping"}"#;

    let mut a = m.connect("disk").unwrap();
    let b = m.connect("disk").unwrap();
    a.send(open.clone()).await;
    b.send(open).await;
    a.send(edit(1, 1, "b")).await;
    b.send(complete).await;
    a.send(edit(2, 2, "c")).await;
    let file = root.path().join(VIRTUAL_DOCUMENTS_DIR).join("nb.ipynb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "abc\n");
    b.send(resolve).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\n");
    b.send(ping.into()).await;
    let mut got = Vec::new();
    loop {
        let m = next(&mut a).await.unwrap();
        if m == ping {
            break;
        }
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        got.push((
            v["method"].clone(),
            v["params"]["contentChanges"][0]["text"].clone(),
        ));
    }
    assert_eq!(
        got,
        [
            (json!("textDocument/didOpen"), json!(null)),
            (json!("textDocument/didChange"), json!("b")),
            (json!("textDocument/didChange"), json!("a\n")),
            (json!("textDocument/completion"), json!(null)),
            (json!("textDocument/didChange"), json!("abc\n")),
            (json!("textDocument/didChange"), json!("a\n")),
            (json!("completionItem/resolve"), json!(null)),
        ]
    );
}

/// Answers each request with `{"id": <its id>, "result": <the request as
/// it arrived>}`, exits on an `exit` notification, and echoes anything
/// else. Perl, which both CI runners
/// have, because `cat` can't answer.
const ANSWERS: &str = r#"
$| = 1; binmode STDIN; binmode STDOUT;
while (1) {
    my $len;
    while (defined(my $l = <STDIN>)) {
        $l =~ s/\r?\n$//;
        last if $l eq '' && defined $len;
        $len = $1 if $l =~ /^Content-Length:\s*(\d+)/i;
    }
    last unless defined $len;
    read(STDIN, my $body, $len) == $len or last;
    last if $body =~ /"method":"exit"/;
    my $out = $body;
    if ($body =~ /"method"/ && $body =~ /"id":(\d+|"[^"]*")/) {
        $out = qq({"id":$1,"jsonrpc":"2.0","result":$body});
    }
    print "Content-Length: " . length($out) . "\r\n\r\n" . $out;
}
"#;

/// Two tabs number their requests alike: each gets the answer to its own
/// request, with its own id, and not the other's (#269).
#[tokio::test]
async fn two_clients_with_the_same_request_id_get_their_own_answers() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("answers", &["perl", "-e", ANSWERS], false)]);
    let mut a = m.connect("answers").unwrap();
    let mut b = m.connect("answers").unwrap();
    let hover = |line: u32| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover",
            "params": {"position": {"line": line, "character": 0}}})
        .to_string()
    };
    a.send(hover(10)).await;
    b.send(hover(20)).await;
    let answer =
        |m: Option<String>| serde_json::from_str::<serde_json::Value>(&m.unwrap()).unwrap();
    let got_a = answer(next(&mut a).await);
    let got_b = answer(next(&mut b).await);
    assert_eq!(got_a["id"], 1);
    assert_eq!(got_b["id"], 1);
    assert_eq!(got_a["result"]["params"]["position"]["line"], 10);
    assert_eq!(got_b["result"]["params"]["position"]["line"], 20);
    // The server saw two different ids.
    assert_ne!(got_a["result"]["id"], got_b["result"]["id"]);

    // Neither got the other's answer: a notification is the next message.
    let ping = r#"{"jsonrpc":"2.0","method":"ping"}"#;
    a.send(ping.into()).await;
    assert_eq!(next(&mut a).await.as_deref(), Some(ping));
    assert_eq!(next(&mut b).await.as_deref(), Some(ping));
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

/// A client of a server that has exited and been started again can still
/// send, and leaves: none of it reaches the new server's documents.
#[tokio::test]
async fn a_client_of_an_ended_server_leaves_the_new_ones_documents_alone() {
    let root = tempfile::tempdir().unwrap();
    let m = manager(root.path(), &[("answers", &["perl", "-e", ANSWERS], true)]);
    let vd = ganymede_lsp::virtual_documents_uri(root.path());
    let uri = format!("{vd}/nb.ipynb");
    let open = |text: &str| {
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "fake", "version": 0, "text": text}}})
        .to_string()
    };
    let edit = json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
        "textDocument": {"uri": uri, "version": 1},
        "contentChanges": [{"range": {"start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0}}, "text": "# x\n"}]}})
    .to_string();
    let ping = r#"{"jsonrpc":"2.0","method":"ping"}"#;

    // The first server ends; its client stays connected.
    let mut old = m.connect("answers").unwrap();
    old.send(r#"{"jsonrpc":"2.0","method":"exit"}"#.into())
        .await;
    assert_eq!(next(&mut old).await, None);

    // A second server, and a client that opens the document.
    let mut new = m.connect("answers").unwrap();
    new.send(open("a = 1\n")).await;
    assert!(next(&mut new).await.unwrap().contains("didOpen"));

    // The old client opens other text, edits, and leaves.
    old.send(open("stale\n")).await;
    old.send(edit.clone()).await;
    drop(old);

    // The new client's edit fits the server's text, so it goes as sent,
    // and the file on disk is its text.
    new.send(edit).await;
    new.send(ping.into()).await;
    let mut got = Vec::new();
    loop {
        let m = next(&mut new).await.unwrap();
        if m == ping {
            break;
        }
        got.push(serde_json::from_str::<serde_json::Value>(&m).unwrap());
    }
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["method"], "textDocument/didChange");
    assert!(
        got[0]["params"]["contentChanges"][0]["range"].is_object(),
        "{got:?}"
    );
    let file = root.path().join(VIRTUAL_DOCUMENTS_DIR).join("nb.ipynb");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# x\na = 1\n");
}
