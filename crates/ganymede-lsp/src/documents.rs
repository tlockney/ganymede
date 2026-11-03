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
//!   copy of an edit), or leaves the client's own text as it is, is
//!   dropped. One made against the server's text goes through as sent;
//!   one made against a client's older text goes as the full text, since
//!   its ranges no longer fit. A change whose range can't be read is
//!   skipped, as a server would skip it;
//! - versions are the relay's own, one more per change the server gets;
//! - `didClose` goes through when the last client closes the document,
//!   and a client that disconnects closes what it had open. When the
//!   client whose text the server has leaves, the server gets the text of
//!   the remaining client that opened or changed it last;
//! - a `didChange` from a client that has the document open only through
//!   other clients is dropped, since there is no text to apply it to;
//! - a request about a document (hover, completion, rename, ...) from a
//!   client whose text the server doesn't have is preceded by that
//!   client's full text, so the answer's positions fit the requester.
//!   A request that follows up on an earlier answer and names no
//!   `textDocument` (`completionItem/resolve`, `codeAction/resolve`, and
//!   the other `*/resolve` methods in [`FOLLOW_UPS`]) is treated as one
//!   about the document the request that returned its item named: the
//!   relay marks each item in the answer with it (see [`tag_items`]).
//!   An item that comes back unmarked goes by the client's last request
//!   of that kind (its last `textDocument/completion` for a completion
//!   item). A call or type hierarchy request is about the
//!   document its `item` names, and a workspace symbol, its `location`'s.
//!   Other requests that name no document (`workspace/symbol`) go as
//!   they are.
//!
//! Tabs that share edits (collaboration) converge, so the server gets
//! each edit once. Tabs that don't send different texts: the server has
//! the text of the tab that changed it last, sent in full.
//!
//! The text the server has is also what the shadow file system writes.
//!
//! Ranges are applied as the server reads them: lines end at `\r\n`,
//! `\r` or `\n`, and `character` counts the position encoding the server
//! chose in its `initialize` result (UTF-16 unless it chose another).

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// What an LSP position's `character` counts (`positionEncoding`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Encoding {
    Utf8,
    /// The protocol's default, and what browsers' strings count.
    #[default]
    Utf16,
    Utf32,
}

impl Encoding {
    /// The `positionEncoding` a server's `initialize` result names;
    /// UTF-16 when it names none or one the protocol doesn't define.
    pub(crate) fn from_lsp(name: Option<&str>) -> Self {
        match name {
            Some("utf-8") => Encoding::Utf8,
            Some("utf-32") => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    fn units(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
            Encoding::Utf32 => 1,
        }
    }
}

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

/// Requests that name no `textDocument` but follow up on an answer about
/// one, with the request whose answer they follow up: resolving a
/// completion item, code action, code lens, inlay hint or document link.
/// `None` where the request carries its document (`params.item.uri` when
/// walking a call or type hierarchy, `params.location.uri` when resolving
/// a workspace symbol).
const FOLLOW_UPS: &[(&str, Option<&str>)] = &[
    ("completionItem/resolve", Some("textDocument/completion")),
    ("codeAction/resolve", Some("textDocument/codeAction")),
    ("codeLens/resolve", Some("textDocument/codeLens")),
    ("inlayHint/resolve", Some("textDocument/inlayHint")),
    ("documentLink/resolve", Some("textDocument/documentLink")),
    ("callHierarchy/incomingCalls", None),
    ("callHierarchy/outgoingCalls", None),
    ("typeHierarchy/supertypes", None),
    ("typeHierarchy/subtypes", None),
    ("workspaceSymbol/resolve", None),
];

/// The key, in an item's `data`, of the document the request that
/// returned the item named. The relay wraps each item's `data` as
/// `{"ganymede.uri": <uri>, "data": <the server's data>}` on its way to the
/// client, which sends it back unchanged when it resolves the item; the
/// relay reads the document from it and restores the server's `data`.
const ITEM_URI: &str = "ganymede.uri";

/// Whether answers to `method` hold items a [`FOLLOW_UPS`] request
/// resolves.
pub(crate) fn returns_items(method: &str) -> bool {
    FOLLOW_UPS.iter().any(|(_, o)| *o == Some(method))
}

/// Whether `method` resolves an item an earlier answer returned.
pub(crate) fn resolves_item(method: &str) -> bool {
    FOLLOW_UPS.iter().any(|(m, o)| *m == method && o.is_some())
}

/// Marks each item in `result` (an answer to a request that
/// [`returns_items`]) with `uri`, the document the request named. Items
/// are the elements of an array result or of a completion list's
/// `items`; a `Command` (whose `command` is a string) is left as it is,
/// since it is never resolved. An item without `data` of its own takes a
/// completion list's `itemDefaults.data`, as the client would.
pub(crate) fn tag_items(result: &mut Value, uri: &str) {
    let default = result
        .get("itemDefaults")
        .and_then(|d| d.get("data"))
        .cloned();
    let items = match result {
        Value::Array(items) => items,
        Value::Object(list) => match list.get_mut("items") {
            Some(Value::Array(items)) => items,
            _ => return,
        },
        _ => return,
    };
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        if item.get("command").is_some_and(Value::is_string) {
            continue;
        }
        let mut tag = serde_json::Map::new();
        tag.insert(ITEM_URI.into(), uri.into());
        if let Some(data) = item.remove("data").or_else(|| default.clone()) {
            tag.insert("data".into(), data);
        }
        item.insert("data".into(), Value::Object(tag));
    }
}

/// The document a resolve's item was returned for, from its tag.
fn item_uri(params: Option<&Value>) -> Option<&str> {
    params?.get("data")?.get(ITEM_URI)?.as_str()
}

/// Removes the tag [`tag_items`] put on a resolve's item, so the server
/// gets its own `data` back.
pub(crate) fn untag_item(params: &mut Value) {
    let Some(item) = params.as_object_mut() else {
        return;
    };
    let Some(Value::Object(mut tag)) = item.remove("data") else {
        return;
    };
    if !tag.contains_key(ITEM_URI) {
        item.insert("data".into(), Value::Object(tag));
    } else if let Some(data) = tag.remove("data") {
        item.insert("data".into(), data);
    }
}

/// Whether `message` (unparsed) may be a request `before_request` acts
/// on: a cheap test, so other messages aren't parsed.
pub(crate) fn may_name_document(message: &str) -> bool {
    message.contains("\"id\"")
        && (message.contains("\"textDocument\"")
            || FOLLOW_UPS.iter().any(|(m, _)| message.contains(m)))
}

struct Doc {
    /// The text and version the server has.
    text: String,
    version: i64,
    /// Each client's text, by client id.
    views: BTreeMap<u64, String>,
    /// When each client last opened or changed it, in `Documents::clock`
    /// ticks.
    touched: BTreeMap<u64, u64>,
}

/// The open documents of one server process.
#[derive(Default)]
pub(crate) struct Documents {
    docs: HashMap<String, Doc>,
    /// Counts opens and changes, to order clients' writes.
    clock: u64,
    /// The document each client's last request of each kind that
    /// [`FOLLOW_UPS`] follow up named, by client and method.
    named: HashMap<(u64, &'static str), String>,
    /// What the server's positions count.
    pub(crate) encoding: Encoding,
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

    /// Before a client's request about a document it has open (one it
    /// names, or for a follow-up in [`FOLLOW_UPS`], the one it carries or
    /// the client's last request it follows up named): when the server's text isn't the
    /// client's, the full-text change that gives the server the client's
    /// text, and the document's URI. `None` when the server already has
    /// it, or for any other message.
    ///
    /// Sends nothing while the tabs agree. Tabs that don't share edits
    /// differ until one changes; tabs that share edits differ only until
    /// an edit reaches both, so a request in that gap sends the older
    /// text, and the requester's own copy of the edit then goes through
    /// in step. It doesn't count as the client's change: when a client
    /// leaves, the server still gets the text changed last.
    pub(crate) fn before_request(
        &mut self,
        client: u64,
        message: &Value,
    ) -> Option<(Value, String)> {
        // As the router: a null id is no id.
        message.get("id").filter(|id| !id.is_null())?;
        let method = message.get("method")?.as_str()?;
        let params = message.get("params");
        let uri = if method.starts_with("textDocument/") {
            let uri = params?.get("textDocument")?.get("uri")?.as_str()?;
            if let Some(origin) = FOLLOW_UPS
                .iter()
                .find_map(|(_, o)| o.filter(|o| *o == method))
            {
                self.named.insert((client, origin), uri.to_string());
            }
            uri.to_string()
        } else {
            let (_, origin) = FOLLOW_UPS.iter().find(|(m, _)| *m == method)?;
            match origin {
                // The document its item came from, or failing that, the one
                // the client's last request of that kind named.
                Some(origin) => match item_uri(params) {
                    Some(uri) => uri.to_string(),
                    None => self.named.get(&(client, *origin))?.clone(),
                },
                // A hierarchy item or workspace symbol carries its document.
                None => {
                    let p = params?;
                    p.get("item")
                        .or_else(|| p.get("location"))?
                        .get("uri")?
                        .as_str()?
                        .to_string()
                }
            }
        };
        let d = self.docs.get_mut(&uri)?;
        let view = d.views.get(&client)?;
        if *view == d.text {
            return None;
        }
        d.text = view.clone();
        d.version += 1;
        Some((full_change(&uri, d.version, &d.text), uri))
    }

    fn open(
        &mut self,
        client: u64,
        uri: &str,
        text: &str,
        doc: Option<&Value>,
    ) -> (Relay, Option<String>) {
        self.clock += 1;
        let now = self.clock;
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
                    touched: BTreeMap::from([(client, now)]),
                },
            );
            return (Relay::AsIs, Some(uri.to_string()));
        };
        d.views.insert(client, text.to_string());
        d.touched.insert(client, now);
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
        let encoding = self.encoding;
        self.clock += 1;
        let now = self.clock;
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
        let mut skipped = false;
        for c in changes {
            let Some(text) = c.get("text").and_then(Value::as_str) else {
                skipped = true;
                continue;
            };
            let r = match c.get("range") {
                None | Some(Value::Null) => None,
                Some(v) => match range(v) {
                    Some(r) => Some(r),
                    // Not a range: there's no telling what it would
                    // replace.
                    None => {
                        skipped = true;
                        continue;
                    }
                },
            };
            after = apply_change(&after, text, r, encoding);
        }
        if after == *before {
            // Nothing changed for this client (an empty change, or one
            // from a client behind the server): the server's text stays.
            return (Relay::Drop, None);
        }
        let in_step = *before == d.text;
        d.views.insert(client, after.clone());
        d.touched.insert(client, now);
        if after == d.text {
            return (Relay::Drop, None);
        }
        d.version += 1;
        d.text = after;
        // A skipped change goes as the text the relay applied, so the
        // server can't read it differently.
        let relay = if in_step && !skipped {
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
        d.touched.remove(&client);
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
        self.named.retain(|(c, _), _| *c != client);
        let (mut send, mut changed) = (Vec::new(), Vec::new());
        self.docs.retain(|uri, d| {
            if d.views.remove(&client).is_none() {
                return true;
            }
            d.touched.remove(&client);
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
    /// change that gives the server the text of the client that opened or
    /// changed it last.
    fn resync(&mut self, uri: &str) -> Option<Value> {
        if self.views.values().any(|v| *v == self.text) {
            return None;
        }
        let last = self
            .views
            .keys()
            .max_by_key(|c| self.touched.get(c).copied().unwrap_or(0))?;
        let text = self.views[last].clone();
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
fn apply_change(
    old: &str,
    text: &str,
    range: Option<((usize, usize), (usize, usize))>,
    encoding: Encoding,
) -> String {
    let Some((start, end)) = range else {
        return text.to_string();
    };
    let a = offset(old, start, encoding);
    let b = offset(old, end, encoding).max(a);
    let mut out = String::with_capacity(old.len() + text.len());
    out.push_str(&old[..a]);
    out.push_str(text);
    out.push_str(&old[b..]);
    out
}

/// The byte offset of an LSP position. Lines end at `\r\n`, `\r` or
/// `\n`, and `character` counts `encoding`'s units. Past the end of a
/// line is its end; past the last line is the end.
fn offset(text: &str, (line, character): (usize, usize), encoding: Encoding) -> usize {
    let mut start = 0;
    for _ in 0..line {
        match line_end(&text[start..]) {
            Some((i, len)) => start += i + len,
            None => return text.len(),
        }
    }
    let end = line_end(&text[start..]).map_or(text.len(), |(i, _)| start + i);
    let mut units = 0;
    for (i, c) in text[start..end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += encoding.units(c);
    }
    end
}

/// Where the first line of `text` ends, and the length of its line end.
fn line_end(text: &str) -> Option<(usize, usize)> {
    let i = text.find(['\r', '\n'])?;
    let len = if text[i..].starts_with("\r\n") { 2 } else { 1 };
    Some((i, len))
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
        let edit = |s, e, text| apply_change(doc, text, Some((s, e)), Encoding::Utf16);
        assert_eq!(edit((0, 7), (0, 9), "sys"), "import sys\nx = 'é😀'\n");
        // 😀 is two UTF-16 units: character 8 is after it.
        assert_eq!(edit((1, 5), (1, 8), "ab"), "import os\nx = 'ab'\n");
        assert_eq!(edit((9, 0), (9, 0), "end"), format!("{doc}end"));
        assert_eq!(edit((0, 99), (1, 0), ""), "import osx = 'é😀'\n");
    }

    #[test]
    fn lines_end_at_crlf_cr_and_lf() {
        for nl in ["\r\n", "\r", "\n"] {
            let doc = format!("a = 1{nl}b = 2{nl}c = 3{nl}");
            let edit = |s, e, text| apply_change(&doc, text, Some((s, e)), Encoding::Utf16);
            assert_eq!(
                edit((1, 4), (1, 5), "9"),
                format!("a = 1{nl}b = 9{nl}c = 3{nl}")
            );
            // Past the end of a line stops before its line end.
            assert_eq!(
                edit((0, 99), (0, 99), "!"),
                format!("a = 1!{nl}b = 2{nl}c = 3{nl}")
            );
            // Joining two lines removes the whole line end.
            assert_eq!(edit((0, 5), (1, 0), ""), format!("a = 1b = 2{nl}c = 3{nl}"));
            assert_eq!(edit((3, 0), (3, 0), "d"), format!("{doc}d"));
        }
        // Mixed, as a file edited on two systems can be.
        let doc = "a\rb\r\nc\nd";
        assert_eq!(
            apply_change(doc, "X", Some(((3, 0), (3, 1))), Encoding::Utf16),
            "a\rb\r\nc\nX"
        );
    }

    #[test]
    fn characters_count_the_servers_encoding() {
        // é is 2 UTF-8 bytes and 1 UTF-16 unit; 😀 is 4 and 2.
        let doc = "x = 'é😀z'\n";
        let z = |enc, at| apply_change(doc, "Z", Some(((0, at), (0, at + 1))), enc);
        assert_eq!(z(Encoding::Utf8, 11), "x = 'é😀Z'\n");
        assert_eq!(z(Encoding::Utf16, 8), "x = 'é😀Z'\n");
        assert_eq!(z(Encoding::Utf32, 7), "x = 'é😀Z'\n");
        assert_eq!(Encoding::from_lsp(Some("utf-8")), Encoding::Utf8);
        assert_eq!(Encoding::from_lsp(Some("utf-32")), Encoding::Utf32);
        assert_eq!(Encoding::from_lsp(Some("utf-16")), Encoding::Utf16);
        assert_eq!(Encoding::from_lsp(None), Encoding::Utf16);

        // A server on UTF-8 gets an edit in step as sent, and the relay's
        // copy matches it.
        let mut d = Documents {
            encoding: Encoding::Utf8,
            ..Documents::default()
        };
        d.client_message(1, &open(doc));
        d.client_message(1, &edit(1, ((0, 11), (0, 12)), "Z"));
        assert_eq!(d.text(URI), Some("x = 'é😀Z'\n"));
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
    fn a_change_that_leaves_the_clients_text_is_dropped() {
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        // Client 2 is behind and sends an empty change: the server keeps
        // client 1's text rather than going back to client 2's.
        let empty = json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": {"uri": URI, "version": 2}, "contentChanges": []}});
        assert_eq!(d.client_message(2, &empty), (Relay::Drop, None));
        assert_eq!(d.text(URI), Some("ab\n"));
    }

    #[test]
    fn a_change_whose_range_cant_be_read_is_skipped() {
        let mut d = Documents::default();
        d.client_message(1, &open("a = 1\n"));
        let bad = |changes: Value| {
            json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                "textDocument": {"uri": URI, "version": 1}, "contentChanges": changes}})
        };
        // Not applied as a full replacement.
        let only_bad = bad(json!([{"range": {"start": "x"}, "text": "zzz"}]));
        assert_eq!(d.client_message(1, &only_bad), (Relay::Drop, None));
        assert_eq!(d.text(URI), Some("a = 1\n"));
        // The readable change beside it still applies.
        let mixed = bad(json!([
            {"range": {"start": "x"}, "text": "zzz"},
            {"range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}},
                "text": "2"}
        ]));
        // It goes to the server as the text the relay applied, so the
        // server can't read the bad range its own way.
        let (relay, _) = d.client_message(1, &mixed);
        assert_eq!(relay, Relay::Replace(full_change(URI, 1, "a = 2\n")));
        assert_eq!(d.text(URI), Some("a = 2\n"));
    }

    #[test]
    fn the_server_gets_the_text_of_the_client_that_changed_it_last() {
        let mut d = Documents::default();
        for c in [1, 2, 3] {
            d.client_message(c, &open("a\n"));
        }
        // Tabs that don't share edits: 2 changes, then 3 changes its older
        // text, so the server has 3's.
        d.client_message(2, &edit(1, ((0, 0), (0, 0)), "2"));
        d.client_message(3, &edit(1, ((0, 0), (0, 0)), "3"));
        assert_eq!(d.text(URI), Some("3a\n"));
        // 3 leaves: the server gets 2's text, the one changed last, not
        // 1's, the lowest id.
        let (send, _) = d.disconnect(3);
        assert_eq!(send, vec![full_change(URI, 3, "2a\n")]);
    }

    #[test]
    fn a_request_goes_against_the_requesters_text() {
        let hover = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover",
            "params": {"textDocument": {"uri": URI}, "position": {"line": 0, "character": 0}}});
        let mut d = Documents::default();
        // Nothing open: nothing to send first.
        assert_eq!(d.before_request(1, &hover), None);
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        // In step: nothing to send.
        assert_eq!(d.before_request(2, &hover), None);
        // Tabs that don't share edits: the server has client 1's text.
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        assert_eq!(d.before_request(1, &hover), None);
        // Client 2 asks: the server gets client 2's text first.
        assert_eq!(
            d.before_request(2, &hover),
            Some((full_change(URI, 2, "a\n"), URI.to_string()))
        );
        assert_eq!(d.text(URI), Some("a\n"));
        assert_eq!(d.before_request(2, &hover), None);
        // A notification, a request with no document, and a client that
        // never opened it send nothing.
        let mut note = hover.clone();
        note.as_object_mut().unwrap().remove("id");
        assert_eq!(d.before_request(1, &note), None);
        let symbols = json!({"jsonrpc": "2.0", "id": 2, "method": "workspace/symbol",
            "params": {"query": "a"}});
        assert_eq!(d.before_request(1, &symbols), None);
        assert_eq!(d.before_request(3, &hover), None);
        // Client 1's next edit, against its own text, goes in full.
        let (r, _) = d.client_message(1, &edit(2, ((0, 2), (0, 2)), "c"));
        assert_eq!(r, Relay::Replace(full_change(URI, 3, "abc\n")));
        // The request didn't count as client 2's change: when client 1
        // leaves, the server gets client 2's text, and when client 2
        // leaves instead, it keeps client 1's.
        let mut leave_2 = Documents::default();
        leave_2.client_message(1, &open("a\n"));
        leave_2.client_message(2, &open("a\n"));
        leave_2.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        leave_2.before_request(2, &hover);
        let (send, _) = leave_2.disconnect(2);
        assert_eq!(send, vec![full_change(URI, 3, "ab\n")]);
    }

    #[test]
    fn a_follow_up_goes_against_the_text_the_requester_last_asked_about() {
        let complete = json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/completion",
            "params": {"textDocument": {"uri": URI}, "position": {"line": 0, "character": 1}}});
        let resolve = json!({"jsonrpc": "2.0", "id": 2, "method": "completionItem/resolve",
            "params": {"label": "abc"}});
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        // No request named a document yet: nothing to send.
        assert_eq!(d.before_request(2, &resolve), None);
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        // Client 2 completes, then client 1 edits, then client 2 resolves.
        assert_eq!(
            d.before_request(2, &complete),
            Some((full_change(URI, 2, "a\n"), URI.to_string()))
        );
        d.client_message(1, &edit(2, ((0, 2), (0, 2)), "c"));
        assert_eq!(d.text(URI), Some("abc\n"));
        assert_eq!(
            d.before_request(2, &resolve),
            Some((full_change(URI, 4, "a\n"), URI.to_string()))
        );
        // A hierarchy request goes by its item's document.
        let calls = json!({"jsonrpc": "2.0", "id": 3, "method": "callHierarchy/incomingCalls",
            "params": {"item": {"uri": URI, "name": "f"}}});
        assert_eq!(
            d.before_request(1, &calls),
            Some((full_change(URI, 5, "abc\n"), URI.to_string()))
        );
        // So does a workspace symbol, by its location's.
        let symbol = json!({"jsonrpc": "2.0", "id": 4, "method": "workspaceSymbol/resolve",
            "params": {"name": "f", "location": {"uri": URI}}});
        assert_eq!(
            d.before_request(2, &symbol),
            Some((full_change(URI, 6, "a\n"), URI.to_string()))
        );
        // A client that leaves forgets what it named.
        d.disconnect(2);
        assert!(d.named.keys().all(|(c, _)| *c != 2));
    }

    /// A resolve follows the completion it resolves, not a request about
    /// another document in between (one connection serves every notebook
    /// in a browser tab).
    #[test]
    fn a_resolve_follows_its_completion_past_other_requests() {
        const OTHER: &str = "file:///other.ipynb";
        let at = |id: i64, method: &str, uri: &str| {
            json!({"jsonrpc": "2.0", "id": id, "method": method,
                "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}})
        };
        let resolve = json!({"jsonrpc": "2.0", "id": 9, "method": "completionItem/resolve",
            "params": {"label": "abc"}});
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        let mut other = open("x\n");
        other["params"]["textDocument"]["uri"] = json!(OTHER);
        d.client_message(2, &other);
        d.before_request(2, &at(1, "textDocument/completion", URI));
        d.before_request(2, &at(2, "textDocument/hover", OTHER));
        // Client 1 edits URI between client 2's completion and its resolve.
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        assert_eq!(d.text(URI), Some("ab\n"));
        assert_eq!(
            d.before_request(2, &resolve),
            Some((full_change(URI, 2, "a\n"), URI.to_string()))
        );
    }

    /// A resolve for an item from a completion on one document, sent
    /// after a completion on another, gets the first document's text
    /// (#339).
    #[test]
    fn a_resolve_goes_by_its_items_document() {
        const OTHER: &str = "file:///other.ipynb";
        let complete = |id: i64, uri: &str| {
            json!({"jsonrpc": "2.0", "id": id, "method": "textDocument/completion",
                "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 0}}})
        };
        let mut item = json!([{"label": "abc"}]);
        tag_items(&mut item, URI);
        let resolve = json!({"jsonrpc": "2.0", "id": 9, "method": "completionItem/resolve",
            "params": item[0]});
        let mut d = Documents::default();
        d.client_message(1, &open("a\n"));
        d.client_message(2, &open("a\n"));
        let mut other = open("x\n");
        other["params"]["textDocument"]["uri"] = json!(OTHER);
        d.client_message(2, &other);
        d.before_request(2, &complete(1, URI));
        d.before_request(2, &complete(2, OTHER));
        // Client 1 edits URI between client 2's completions and its resolve.
        d.client_message(1, &edit(1, ((0, 1), (0, 1)), "b"));
        assert_eq!(d.text(URI), Some("ab\n"));
        assert_eq!(
            d.before_request(2, &resolve),
            Some((full_change(URI, 2, "a\n"), URI.to_string()))
        );
    }

    #[test]
    fn may_name_document_picks_out_requests_about_documents() {
        assert!(may_name_document(
            r#"{"id":1,"method":"textDocument/hover","params":{"textDocument":{}}}"#
        ));
        assert!(may_name_document(
            r#"{"id":1,"method":"completionItem/resolve","params":{}}"#
        ));
        assert!(!may_name_document(
            r#"{"method":"completionItem/resolve","params":{}}"#
        ));
        assert!(!may_name_document(
            r#"{"id":1,"method":"workspace/symbol","params":{}}"#
        ));
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
