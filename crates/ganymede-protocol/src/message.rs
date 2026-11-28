//! The message envelope.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: &str = "5.5";

/// The five kernel channels. Heartbeat carries no messages and is handled
/// by transports directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Shell,
    Control,
    #[serde(rename = "iopub")]
    IoPub,
    Stdin,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Shell => "shell",
            Channel::Control => "control",
            Channel::IoPub => "iopub",
            Channel::Stdin => "stdin",
        }
    }
}

impl std::str::FromStr for Channel {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "shell" => Channel::Shell,
            "control" => Channel::Control,
            "iopub" => Channel::IoPub,
            "stdin" => Channel::Stdin,
            other => return Err(crate::Error::UnknownChannel(other.to_string())),
        })
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Message header. `date` is kept as the string the peer sent so that
/// nothing is lost in a round trip; [`Header::new`] produces RFC 3339.
///
/// `username`, `date` and `version` default to `""` when a peer leaves
/// them out: `jupyter_client` does not require them, and rejecting such a
/// header would drop the reply the host is waiting for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub msg_id: String,
    pub session: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub date: String,
    pub msg_type: String,
    #[serde(default)]
    pub version: String,
    /// Protocol 5.5: present when the message targets a subshell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subshell_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `t` as `YYYY-MM-DDTHH:MM:SS.ffffffZ`, the form a Jupyter header's
/// `date` takes: UTC, six fraction digits (truncated, not rounded), `Z`.
fn iso_micros(t: jiff::Timestamp) -> String {
    t.strftime("%Y-%m-%dT%H:%M:%S.%6fZ").to_string()
}

impl Header {
    pub fn new(
        msg_type: impl Into<String>,
        session: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Header {
            msg_id: uuid::Uuid::new_v4().to_string(),
            session: session.into(),
            username: username.into(),
            date: iso_micros(jiff::Timestamp::now()),
            msg_type: msg_type.into(),
            version: PROTOCOL_VERSION.to_string(),
            subshell_id: None,
            extra: Map::new(),
        }
    }
}

/// A complete message. `content` is kept as JSON; use
/// [`Message::content_as`] to read it as a typed struct from [`crate::content`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub header: Header,
    /// An empty object on the wire when there is no parent. Modelled as
    /// `Option` and serialized as `{}` when `None`.
    #[serde(
        default,
        deserialize_with = "deserialize_parent",
        serialize_with = "serialize_parent"
    )]
    pub parent_header: Option<Header>,
    /// `null` is read as `{}`.
    #[serde(default, deserialize_with = "deserialize_metadata")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub content: Value,
    #[serde(default, skip)]
    pub buffers: Vec<Bytes>,
}

fn deserialize_parent<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Header>, D::Error> {
    let v = Value::deserialize(d)?;
    match v {
        Value::Null => Ok(None),
        Value::Object(ref m) if m.is_empty() => Ok(None),
        other => serde_json::from_value(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn deserialize_metadata<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Map<String, Value>, D::Error> {
    Ok(Option::<Map<String, Value>>::deserialize(d)?.unwrap_or_default())
}

/// A metadata frame: a JSON object, with `null` read as `{}`.
pub(crate) fn parse_metadata(b: &[u8]) -> crate::Result<Map<String, Value>> {
    Ok(serde_json::from_slice::<Option<Map<String, Value>>>(b)?.unwrap_or_default())
}

fn serialize_parent<S: serde::Serializer>(p: &Option<Header>, s: S) -> Result<S::Ok, S::Error> {
    match p {
        Some(h) => h.serialize(s),
        None => Map::<String, Value>::new().serialize(s),
    }
}

impl Message {
    /// Build a new message with a fresh header.
    pub fn new<C: Serialize>(msg_type: &str, session: &str, username: &str, content: &C) -> Self {
        Message {
            header: Header::new(msg_type, session, username),
            parent_header: None,
            metadata: Map::new(),
            content: serde_json::to_value(content).expect("content serializes"),
            buffers: Vec::new(),
        }
    }

    /// Build a reply or iopub message whose parent is `parent`.
    pub fn reply_to<C: Serialize>(parent: &Message, msg_type: &str, content: &C) -> Self {
        let mut m = Message::new(
            msg_type,
            &parent.header.session,
            &parent.header.username,
            content,
        );
        m.parent_header = Some(parent.header.clone());
        m
    }

    pub fn with_parent(mut self, parent: Header) -> Self {
        self.parent_header = Some(parent);
        self
    }

    pub fn with_buffers(mut self, buffers: Vec<Bytes>) -> Self {
        self.buffers = buffers;
        self
    }

    pub fn msg_type(&self) -> &str {
        &self.header.msg_type
    }

    pub fn msg_id(&self) -> &str {
        &self.header.msg_id
    }

    /// The parent's msg_id, which is how replies and iopub output are
    /// correlated with requests.
    pub fn parent_id(&self) -> Option<&str> {
        self.parent_header.as_ref().map(|h| h.msg_id.as_str())
    }

    pub fn content_as<T: serde::de::DeserializeOwned>(&self) -> crate::Result<T> {
        Ok(serde_json::from_value(self.content.clone())?)
    }
}

/// `msg` in the websocket's JSON shape, with its buffers as base64
/// strings in `buffers`. This is what the C ABI and the kernel provider
/// socket carry.
pub fn message_to_json(msg: &Message) -> crate::Result<Value> {
    use base64::Engine;
    let mut v = serde_json::to_value(msg)?;
    v["buffers"] = msg
        .buffers
        .iter()
        .map(|b| base64::engine::general_purpose::STANDARD.encode(b).into())
        .collect::<Vec<Value>>()
        .into();
    Ok(v)
}

/// The other way from [`message_to_json`]: base64 strings in `buffers`
/// become the buffers.
pub fn message_from_json(mut v: Value) -> crate::Result<Message> {
    use base64::Engine;
    let buffers = match v.as_object_mut().and_then(|m| m.remove("buffers")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|b| {
                let text = b
                    .as_str()
                    .ok_or(crate::Error::Malformed("a buffer is not a base64 string"))?;
                Ok(base64::engine::general_purpose::STANDARD
                    .decode(text)?
                    .into())
            })
            .collect::<crate::Result<_>>()?,
        Some(_) => return Err(crate::Error::Malformed("buffers is not a list")),
    };
    if let Some(m) = v.as_object_mut() {
        m.remove("channel");
    }
    let mut msg: Message = serde_json::from_value(v)?;
    msg.buffers = buffers;
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip_preserves_unknown_fields() {
        let j = r#"{"msg_id":"a","session":"s","username":"u","date":"2026-10-17T00:00:00Z","msg_type":"status","version":"5.5","x_capabilities":{"jit":false}}"#;
        let h: Header = serde_json::from_str(j).unwrap();
        assert_eq!(h.extra["x_capabilities"]["jit"], Value::Bool(false));
        let back = serde_json::to_string(&h).unwrap();
        assert!(back.contains("x_capabilities"));
        assert!(!back.contains("subshell_id"));
    }

    #[test]
    fn empty_parent_is_none_and_serializes_as_empty_object() {
        let j = r#"{"header":{"msg_id":"a","session":"s","username":"u","date":"d","msg_type":"t","version":"5.5"},"parent_header":{},"metadata":{},"content":{}}"#;
        let m: Message = serde_json::from_str(j).unwrap();
        assert!(m.parent_header.is_none());
        let back = serde_json::to_value(&m).unwrap();
        assert_eq!(back["parent_header"], serde_json::json!({}));
    }

    #[test]
    fn header_without_username_date_version_defaults_to_empty() {
        let j = r#"{"msg_id":"a","session":"s","msg_type":"status"}"#;
        let h: Header = serde_json::from_str(j).unwrap();
        assert_eq!(h.username, "");
        assert_eq!(h.date, "");
        assert_eq!(h.version, "");
        assert!(serde_json::from_str::<Header>(r#"{"session":"s","msg_type":"t"}"#).is_err());
    }

    #[test]
    fn null_metadata_is_empty() {
        let j = r#"{"header":{"msg_id":"a","session":"s","msg_type":"t"},"parent_header":null,"metadata":null,"content":{}}"#;
        let m: Message = serde_json::from_str(j).unwrap();
        assert!(m.metadata.is_empty());
        assert!(parse_metadata(b"null").unwrap().is_empty());
        assert!(parse_metadata(b"[]").is_err());
    }
}

#[cfg(test)]
mod iso_tests {
    use super::*;

    /// Fixed instants, so the format is pinned independent of the clock. The
    /// expected strings are what chrono's formatting gave before the port.
    #[test]
    fn format_is_pinned() {
        // 2026-01-02T03:04:05Z
        let at = |nanos: i32| jiff::Timestamp::new(1_767_323_045, nanos).unwrap();
        for (nanos, want) in [
            (0, "2026-01-02T03:04:05.000000Z"),
            (123000000, "2026-01-02T03:04:05.123000Z"),
            (123456000, "2026-01-02T03:04:05.123456Z"),
            (123456789, "2026-01-02T03:04:05.123456Z"),
            (1000, "2026-01-02T03:04:05.000001Z"),
            (100, "2026-01-02T03:04:05.000000Z"),
            (999999999, "2026-01-02T03:04:05.999999Z"),
        ] {
            assert_eq!(iso_micros(at(nanos)), want, "nanos {nanos}");
        }
    }
}
