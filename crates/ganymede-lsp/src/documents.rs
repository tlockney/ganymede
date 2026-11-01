//! One view of each open document for a server shared by several clients.
//!
//! Two tabs (or two people) on one notebook each open its virtual
//! document and each send the same edits. Relayed as they are, the server
//! gets the document opened twice and every edit applied twice, so its
//! text drifts from the editors'. Here each client's text is tracked, and
//! the server gets one document per URI:
//!
//! - the first `didOpen` goes through; a later client's `didOpen` becomes
//!   a full-text `didChange` when its text differs, and is dropped when
//!   it doesn't;
//! - a `didChange` that leaves the server's text as it is (the second
//!   copy of an edit) is dropped. One made against the server's text goes
//!   through as sent; one made against a client's older text goes as the
//!   full text, since its ranges no longer fit;
//! - versions are the relay's own, one more per change the server gets;
//! - `didClose` goes through when the last client closes the document,
//!   and a client that disconnects closes what it had open. When the
//!   client whose text the server has leaves, the server gets another
//!   client's text;
//! - a `didChange` from a client that has the document open only through
//!   other clients is dropped, since there is no text to apply it to.
//!
//! Tabs that share edits (collaboration) converge, so the server gets
//! each edit once. Tabs that don't send different texts: the server has
//! the text of the tab that changed it last, sent in full.
//!
//! The text the server has is also what the shadow file system writes.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// What to do with a client's message.
#[derive(Debug, PartialEq)]
pub(crate) enum Relay {
    /// Send it to the server as the client sent it.
    AsIs,
    /// Send this instead.
    Replace(Value),
    /// Send nothing.
    Drop,
}

struct Doc {
    /// The text and version the server has.
    text: String,
    version: i64,
    /// Each client's text, by client id.
    views: BTreeMap<u64, String>,
}

/// The open documents of one server process.
#[derive(Default)]
pub(crate) struct Documents {
    docs: HashMap<String, Doc>,
}

impl Documents {
    /// The server's text of `uri`.
    pub(crate) fn text(&self, uri: &str) -> Option<&str> {
        self.docs.get(uri).map(|d| d.text.as_str())
    }

    /// Handles a client's message, and returns what to send and, when the
    /// server's text of a document changed, its URI.
    pub(crate) fn client_message(
        &mut self,
        client: u64,
        message: &Value,
    ) -> (Relay, Option<String>) {
        let method = message.get("method").and_then(Value::as_str);
        let params = message.get("params");
        let doc = params.and_then(|p| p.get("textDocument"));
        let Some(uri) = doc.and_then(|d| d.get("uri")).and_then(Value::as_str) else {
            return (Relay::AsIs, None);
        };
        match method {
            Some("textDocument/didOpen") => {
                let Some(text) = doc.and_then(|d| d.get("text")).and_then(Value::as_str) else {
                    return (Relay::AsIs, None);
                };
                self.open(client, uri, text, doc)
            }
            Some("textDocument/didChange") => {
                let changes = params
                    .and_then(|p| p.get("contentChanges"))
                    .and_then(Value::as_array);
                match changes {
                    Some(c) => self.change(client, uri, c, message),
                    None => (Relay::AsIs, None),
                }
            }
            Some("textDocument/didClose") => self.close(client, uri),
            _ => (Relay::AsIs, None),
        }
    }

    fn open(
        &mut self,
        client: u64,
        uri: &str,
        text: &str,
        doc: Option<&Value>,
    ) -> (Relay, Option<String>) {
        let Some(d) = self.docs.get_mut(uri) else {
            let version = doc
                .and_then(|d| d.get("version"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.docs.insert(
                uri.to_string(),
                Doc {
                    text: text.to_string(),
                    version,
                    views: BTreeMap::from([(client, text.to_string())]),
                },
            );
            return (Relay::AsIs, Some(uri.to_string()));
        };
        d.views.insert(client, text.to_string());
        if d.text == text {
            return (Relay::Drop, None);
        }
        d.text = text.to_string();
        d.version += 1;
        (
            Relay::Replace(full_change(uri, d.version, text)),
            Some(uri.to_string()),
        )
    }

    fn change(
        &mut self,
        client: u64,
        uri: &str,
        changes: &[Value],
        message: &Value,
    ) -> (Relay, Option<String>) {
        let Some(d) = self.docs.get_mut(uri) else {
            return (Relay::AsIs, None);
        };
        let Some(before) = d.views.get(&client) else {
            // The server has the document open through other clients, so
            // it would take this change, but there's no text here to apply
            // it to, and the two copies would part.
            return (Relay::Drop, None);
        };
        let mut after = before.clone();
        for c in changes {
            let Some(text) = c.get("text").and_then(Value::as_str) else {
                continue;
            };
            after = apply_change(&after, text, c.get("range").and_then(range));
        }
        let in_step = *before == d.text;
        d.views.insert(client, after.clone());
        if after == d.text {
            return (Relay::Drop, None);
        }
        d.version += 1;
        d.text = after;
        let relay = if in_step {
            let mut m = message.clone();
            m["params"]["textDocument"]["version"] = d.version.into();
            Relay::Replace(m)
        } else {
            Relay::Replace(full_change(uri, d.version, &d.text))
        };
        (relay, Some(uri.to_string()))
    }

    fn close(&mut self, client: u64, uri: &str) -> (Relay, Option<String>) {
        let Some(d) = self.docs.get_mut(uri) else {
            return (Relay::AsIs, None);
        };
        d.views.remove(&client);
        if d.views.is_empty() {
            self.docs.remove(uri);
            return (Relay::AsIs, None);
        }
        match d.resync(uri) {
            Some(m) => (Relay::Replace(m), Some(uri.to_string())),
            None => (Relay::Drop, None),
        }
    }

    /// A client left: what to send the server (a `didClose` for each
    /// document only it had open, a full text for each whose server text
    /// was its), and the URIs whose server text changed.
    pub(crate) fn disconnect(&mut self, client: u64) -> (Vec<Value>, Vec<String>) {
        let (mut send, mut changed) = (Vec::new(), Vec::new());
        self.docs.retain(|uri, d| {
            if d.views.remove(&client).is_none() {
                return true;
            }
            if d.views.is_empty() {
                send.push(json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didClose",
                    "params": {"textDocument": {"uri": uri}},
                }));
                return false;
            }
            if let Some(m) = d.resync(uri) {
                send.push(m);
                changed.push(uri.clone());
            }
            true
        });
        (send, changed)
    }
}

impl Doc {
    /// When no remaining client has the server's text, the full-text
    /// change that gives the server the first one's.
    fn resync(&mut self, uri: &str) -> Option<Value> {
        if self.views.values().any(|v| *v == self.text) {
            return None;
        }
        let text = self.views.values().next()?.clone();
        self.version += 1;
        self.text = text;
        Some(full_change(uri, self.version, &self.text))
    }
}

fn full_change(uri: &str, version: i64, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": text}],
        },
    })
}

/// `{"start": {"line", "character"}, "end": {...}}`.
fn range(v: &Value) -> Option<((usize, usize), (usize, usize))> {
    let pos = |p: &Value| -> Option<(usize, usize)> {
        Some((
            usize::try_from(p.get("line")?.as_u64()?).ok()?,
            usize::try_from(p.get("character")?.as_u64()?).ok()?,
        ))
    };
    Some((pos(v.get("start")?)?, pos(v.get("end")?)?))
}

/// Replaces the range (all of `old` without one) with `text`.
fn apply_change(old: &str, text: &str, range: Option<((usize, usize), (usize, usize))>) -> String {
    let Some((start, end)) = range else {
        return text.to_string();
    };
    let a = offset(old, start);
    let b = offset(old, end).max(a);
    let mut out = String::with_capacity(old.len() + text.len());
    out.push_str(&old[..a]);
    out.push_str(text);
    out.push_str(&old[b..]);
    out
}

/// The byte offset of an LSP position. `character` counts UTF-16 code
/// units, as the protocol's default encoding and the browser's strings
/// do. Past the end of a line is its end; past the last line is the end.
fn offset(text: &str, (line, character): (usize, usize)) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (i, c) in text[start..line_end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += c.len_utf16();
    }
    line_end
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "file:///r/.virtual_documents/nb.ipynb";

    fn open(text: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {
            "uri": URI, "languageId": "python", "version": 0, "text": text}}})
    }

    fn edit(version: i64, (s, e): ((usize, usize), (usize, usize)), text: &str) -> Value {
        json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": URI, "version": version},
            "contentChanges": [{"range": {"start": {"line": s.0, "character": s.1},
                "end": {"line": e.0, "character": e.1}}, "text": text}]}})
    }

    fn close() -> Value {
        json!({"jsonrpc": "2.0", "method": "textDocument/didClose",
            "params": {"textDocument": {"uri": URI}}})
    }

    #[test]
    fn range_edits_count_utf16() {
        let doc = "import os\nx = 'é😀'\n";
        let edit = |s, e, text| apply_change(doc, text, Some((s, e)));
        assert_eq!(edit((0, 7), (0, 9), "sys"), "import sys\nx = 'é😀'\n");
        // 😀 is two UTF-16 units: character 8 is after it.
        assert_eq!(edit((1, 5), (1, 8), "ab"), "import os\nx = 'ab'\n");
        assert_eq!(edit((9, 0), (9, 0), "end"), format!("{doc}end"));
        assert_eq!(edit((0, 99), (1, 0), ""), "import osx = 'é😀'\n");
    }

    #[test]
    fn one_client_goes_through_with_the_relays_versions() {
        let mut d = Documents::default();
        let (r, w) = d.client_message(1, &open("a = 1\n"));
        assert_eq!(r, Relay::AsIs);
        assert_eq!(w.as_deref(), Some(URI));
        assert_eq!(d.text(URI), Some("a = 1\n"));
        let (r, w) = d.client_message(1, &edit(7, ((0, 4), (0, 5)), "2"));
        assert_eq!(r, Relay::Replace(edit(1, ((0, 4), (0, 5)), "2")));
        assert_eq!(w.as_deref(), Some(URI));
        assert_eq!(d.text(URI), Some("a = 2\n"));
        assert_eq!(d.client_message(1, &close()).0, Relay::AsIs);
        assert!(d.docs.is_empty());
    }

    #[test]
    fn the_same_edit_from_two_clients_applies_once() {
        let mut d = Documents::default();
        d.client_message(1, &open("a = 1\n"));
        // The second tab opens the same text: the server has it already.
        assert_eq!(d.client_message(2, &open("a = 1\n")), (Relay::Drop, None));
        let e = edit(1, ((0, 0), (0, 0)), "# x\n");
        assert_eq!(d.client_message(1, &e).1.as_deref(), Some(URI));
        // The same edit, from the other tab.
        assert_eq!(d.client_message(2, &e), (Relay::Drop, None));
        assert_eq!(d.docs[URI].text, "# x\na = 1\n");
        assert_eq!(d.docs[URI].version, 1);
    }

    #[test]
    fn a_client_behind_sends_its_full_text() {
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        // Client 2 edits its older text: its range doesn't fit the
        // server's, so its result goes in full.
        let (r, w) = d.client_message(2, &edit(1, ((0, 0), (0, 0)), "x"));
        assert_eq!(r, Relay::Replace(full_change(URI, 2, "xa\n")));
        assert_eq!(w.as_deref(), Some(URI));
        assert_eq!(d.text(URI), Some("xa\n"));
        // A client opening different text replaces the server's.
        let (r, _) = d.client_message(3, &open("z\n"));
        assert_eq!(r, Relay::Replace(full_change(URI, 3, "z\n")));
    }

    #[test]
    fn closes_when_the_last_client_does() {
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        assert_eq!(d.client_message(1, &close()).0, Relay::Drop);
        assert_eq!(d.disconnect(1), (vec![], vec![]));
        let (closed, changed) = d.disconnect(2);
        assert_eq!(closed.len(), 1);
        assert!(changed.is_empty());
        assert_eq!(closed[0]["params"]["textDocument"]["uri"], URI);
        // Now a stray close goes through untouched.
        assert_eq!(d.client_message(2, &close()).0, Relay::AsIs);
    }

    #[test]
    fn other_messages_pass() {
        let mut d = Documents::default();
        let hover = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": URI}}});
        assert_eq!(d.client_message(1, &hover), (Relay::AsIs, None));
        // Edits to a document nobody opened here.
        assert_eq!(
            d.client_message(1, &edit(1, ((0, 0), (0, 0)), "x")),
            (Relay::AsIs, None)
        );
        // Edits from a client that never opened a document others have.
        d.client_message(1, &open("a\n"));
        assert_eq!(
            d.client_message(2, &edit(1, ((0, 0), (0, 0)), "x")),
            (Relay::Drop, None)
        );
        assert_eq!(d.text(URI), Some("a\n"));
    }

    #[test]
    fn the_server_follows_a_remaining_client_when_its_text_leaves() {
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        // Tabs that don't share edits: the server has client 1's text.
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        assert_eq!(d.text(URI), Some("ab\n"));
        let (send, changed) = d.disconnect(1);
        assert_eq!(send, vec![full_change(URI, 2, "a\n")]);
        assert_eq!(changed, vec![URI.to_string()]);
        // The same on close.
        d.client_message(3, &open("c\n"));
        assert_eq!(
            d.client_message(3, &close()),
            (Relay::Replace(full_change(URI, 4, "a\n")), Some(URI.into()))
        );
    }
}
