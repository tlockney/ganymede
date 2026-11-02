//! JSON-RPC ids between many clients and one shared server (#269).
//!
//! Every `jupyterlab-lsp` client numbers its requests from its own
//! counter, so two tabs on one server send requests with the same id. The
//! relay gives each client request an id of its own on the way to the
//! server, and sends the response back to that client alone, with the
//! client's id restored. A request from the server (`workspace/configuration`,
//! `client/registerCapability`) goes to one client, whose answer is the
//! one the server gets. Notifications go to every client, as before.
//!
//! The answer to `initialize` also says which position encoding the server
//! chose, which the relay's document copy counts in (see `documents.rs`).

use crate::documents::Encoding;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Where a message from the server goes.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// Every client: a notification, or a message the relay can't read.
    All,
    /// One client.
    To(u64, Arc<str>),
    /// No client: the answer to a request whose client has left.
    Nowhere,
}

#[derive(Default)]
pub struct Router {
    next: u64,
    /// The relay's id for a client request, to the client and its own id.
    pending: HashMap<u64, (u64, Value)>,
    /// A server request's id (as JSON text), to the client asked and the
    /// request, so it can be asked of another client if that one leaves.
    asked: HashMap<String, (u64, Arc<str>)>,
    /// The relay's ids of `initialize` requests not yet answered.
    initializing: HashSet<u64>,
    /// The position encoding the server chose.
    encoding: Encoding,
}

impl Router {
    /// A message from `client`, rewritten for the server; `None` when it
    /// goes nowhere (an answer to a server request asked of another
    /// client, or a cancel for a request already answered).
    pub fn client_message(&mut self, client: u64, message: String) -> Option<String> {
        let Ok(mut v) = serde_json::from_str::<Value>(&message) else {
            return Some(message);
        };
        let Some(obj) = v.as_object_mut() else {
            return Some(message);
        };
        let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);
        match (obj.get("id").filter(|id| !id.is_null()).cloned(), method) {
            // A request: give it the relay's own id.
            (Some(id), Some(method)) => {
                self.next += 1;
                self.pending.insert(self.next, (client, id));
                if method == "initialize" {
                    self.initializing.insert(self.next);
                }
                obj.insert("id".into(), self.next.into());
                Some(v.to_string())
            }
            // An answer to a server request: only from the client asked.
            (Some(id), None) => {
                let key = id.to_string();
                match self.asked.get(&key) {
                    Some((asked, _)) if *asked == client => {
                        self.asked.remove(&key);
                        Some(message)
                    }
                    _ => None,
                }
            }
            (None, Some(m)) if m == "$/cancelRequest" => {
                let theirs = obj.get("params").and_then(|p| p.get("id")).cloned()?;
                let ours = self
                    .pending
                    .iter()
                    .find(|(_, (c, id))| *c == client && *id == theirs)
                    .map(|(ours, _)| *ours)?;
                v["params"]["id"] = ours.into();
                Some(v.to_string())
            }
            _ => Some(message),
        }
    }

    /// Where a message from the server goes. `clients` are the clients
    /// connected now, in the order to ask them.
    pub fn server_message(&mut self, message: &str, clients: &[u64]) -> Route {
        let Ok(mut v) = serde_json::from_str::<Value>(message) else {
            return Route::All;
        };
        let Some(obj) = v.as_object_mut() else {
            return Route::All;
        };
        let id = obj.get("id").filter(|id| !id.is_null()).cloned();
        match (id, obj.contains_key("method")) {
            // A request from the server: ask one client.
            (Some(id), true) => {
                let Some(&client) = clients.first() else {
                    return Route::Nowhere;
                };
                let message: Arc<str> = message.into();
                self.asked.insert(id.to_string(), (client, message.clone()));
                Route::To(client, message)
            }
            // A response: to the client whose request it answers.
            (Some(id), false) => {
                let ours = id.as_u64();
                // Only a result says what the server counts in: an error
                // (a second `initialize` to a running server) changes
                // nothing.
                let result = obj.get("result").filter(|r| !r.is_null());
                if ours.is_some_and(|o| self.initializing.remove(&o)) && result.is_some() {
                    let chosen = result
                        .and_then(|r| r.get("capabilities"))
                        .and_then(|c| c.get("positionEncoding"))
                        .and_then(Value::as_str);
                    self.encoding = Encoding::from_lsp(chosen);
                }
                let Some((client, theirs)) = ours.and_then(|ours| self.pending.remove(&ours))
                else {
                    return Route::Nowhere;
                };
                obj.insert("id".into(), theirs);
                Route::To(client, v.to_string().into())
            }
            _ => Route::All,
        }
    }

    /// The position encoding the server chose in its `initialize` result.
    pub(crate) fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// `client` left: answers to its requests go nowhere, and what the
    /// server asked of it is asked of the next client in `remaining`.
    pub fn disconnect(&mut self, client: u64, remaining: &[u64]) -> Vec<(u64, Arc<str>)> {
        self.pending.retain(|_, (c, _)| *c != client);
        let Some(&next) = remaining.first() else {
            self.asked.retain(|_, (c, _)| *c != client);
            return Vec::new();
        };
        let mut resend = Vec::new();
        for (asked, message) in self.asked.values_mut() {
            if *asked == client {
                *asked = next;
                resend.push((next, message.clone()));
            }
        }
        resend
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn to(route: Route) -> (u64, Value) {
        match route {
            Route::To(c, m) => (c, serde_json::from_str(&m).unwrap()),
            other => panic!("expected one client, got {other:?}"),
        }
    }

    fn sent(m: Option<String>) -> Value {
        serde_json::from_str(&m.expect("sent")).unwrap()
    }

    #[test]
    fn same_request_id_from_two_clients_answers_each_alone() {
        let mut r = Router::default();
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "textDocument/hover"}).to_string();
        let a = sent(r.client_message(1, req.clone()));
        let b = sent(r.client_message(2, req));
        assert_ne!(a["id"], b["id"]);

        // Answered in the other order.
        let reply =
            |id: &Value, who: &str| json!({"jsonrpc": "2.0", "id": id, "result": who}).to_string();
        let (client, m) = to(r.server_message(&reply(&b["id"], "b"), &[1, 2]));
        assert_eq!(
            (client, m["id"].clone(), m["result"].clone()),
            (2, json!(5), json!("b"))
        );
        let (client, m) = to(r.server_message(&reply(&a["id"], "a"), &[1, 2]));
        assert_eq!(
            (client, m["id"].clone(), m["result"].clone()),
            (1, json!(5), json!("a"))
        );
        // A second answer to the same request goes nowhere.
        assert_eq!(
            r.server_message(&reply(&a["id"], "a"), &[1, 2]),
            Route::Nowhere
        );
    }

    #[test]
    fn string_ids_come_back_as_strings() {
        let mut r = Router::default();
        let ours = sent(r.client_message(1, json!({"id": "x", "method": "m"}).to_string()));
        let (_, m) =
            to(r.server_message(&json!({"id": ours["id"], "result": null}).to_string(), &[1]));
        assert_eq!(m["id"], "x");
    }

    #[test]
    fn notifications_go_to_all_and_pass_unchanged() {
        let mut r = Router::default();
        let n = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{}}"#;
        assert_eq!(r.server_message(n, &[1, 2]), Route::All);
        assert_eq!(r.server_message("not json", &[1, 2]), Route::All);
        assert_eq!(r.client_message(1, n.into()).as_deref(), Some(n));
    }

    #[test]
    fn a_server_request_is_asked_of_one_client() {
        let mut r = Router::default();
        let ask =
            json!({"jsonrpc": "2.0", "id": 0, "method": "workspace/configuration"}).to_string();
        let (client, _) = to(r.server_message(&ask, &[1, 2]));
        assert_eq!(client, 1);
        let answer = json!({"jsonrpc": "2.0", "id": 0, "result": [{}]}).to_string();
        assert_eq!(r.client_message(2, answer.clone()), None);
        assert_eq!(
            r.client_message(1, answer.clone()).as_deref(),
            Some(&*answer)
        );
        // Answered once.
        assert_eq!(r.client_message(1, answer), None);
    }

    #[test]
    fn a_leaving_client_hands_on_what_it_was_asked() {
        let mut r = Router::default();
        let ask = json!({"id": 3, "method": "client/registerCapability"}).to_string();
        to(r.server_message(&ask, &[1, 2]));
        let mine = sent(r.client_message(1, json!({"id": 9, "method": "m"}).to_string()));
        let resend = r.disconnect(1, &[2]);
        assert_eq!(resend, vec![(2, Arc::from(ask.as_str()))]);
        assert_eq!(
            r.server_message(&json!({"id": mine["id"], "result": 1}).to_string(), &[2]),
            Route::Nowhere
        );
        let answer = json!({"id": 3, "result": null}).to_string();
        assert!(r.client_message(2, answer).is_some());
    }

    #[test]
    fn the_initialize_result_sets_the_encoding() {
        let mut r = Router::default();
        assert_eq!(r.encoding(), Encoding::Utf16);
        let init = |r: &mut Router| {
            sent(r.client_message(1, json!({"id": 0, "method": "initialize"}).to_string()))
        };
        let ours = init(&mut r);
        let answer_to = |id: &Value, enc: Value| {
            json!({"id": id, "result": {"capabilities": {"positionEncoding": enc}}}).to_string()
        };
        let answer = |enc: Value| answer_to(&ours["id"], enc);
        // Another response doesn't count.
        let other = sent(r.client_message(1, json!({"id": 1, "method": "m"}).to_string()));
        r.server_message(
            &json!({"id": other["id"], "result": {"capabilities": {"positionEncoding": "utf-8"}}})
                .to_string(),
            &[1],
        );
        assert_eq!(r.encoding(), Encoding::Utf16);
        to(r.server_message(&answer(json!("utf-8")), &[1]));
        assert_eq!(r.encoding(), Encoding::Utf8);
        // A second client's initialize that the running server refuses
        // leaves the server's choice as it was.
        let ours = init(&mut r);
        to(r.server_message(
            &json!({"id": ours["id"], "error": {"code": -32600, "message": "initialized"}})
                .to_string(),
            &[1],
        ));
        assert_eq!(r.encoding(), Encoding::Utf8);
        // The server's choice counts even when the client that asked has
        // left: the encoding is the server's, not the client's.
        let ours = sent(r.client_message(2, json!({"id": 0, "method": "initialize"}).to_string()));
        r.disconnect(2, &[1]);
        assert_eq!(
            r.server_message(&answer_to(&ours["id"], json!("utf-32")), &[1]),
            Route::Nowhere
        );
        assert_eq!(r.encoding(), Encoding::Utf32);
        // A later initialize without one is back to the default.
        let ours = init(&mut r);
        to(r.server_message(
            &json!({"id": ours["id"], "result": {"capabilities": {}}}).to_string(),
            &[1],
        ));
        assert_eq!(r.encoding(), Encoding::Utf16);
    }

    #[test]
    fn cancel_names_the_relays_id() {
        let mut r = Router::default();
        r.client_message(1, json!({"id": 4, "method": "a"}).to_string());
        let ours = sent(r.client_message(2, json!({"id": 4, "method": "b"}).to_string()));
        let cancel = json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 4}});
        let c = sent(r.client_message(2, cancel.to_string()));
        assert_eq!(c["params"]["id"], ours["id"]);
        // A cancel for a request already answered goes nowhere.
        r.server_message(&json!({"id": ours["id"], "result": 0}).to_string(), &[1, 2]);
        assert_eq!(r.client_message(2, cancel.to_string()), None);
    }
}
