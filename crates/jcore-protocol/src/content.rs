//! Typed content for the messages a host sends and reads.
//!
//! Every struct carries `#[serde(flatten)] extra` so fields this crate does
//! not model survive a round trip. Only the messages a host needs are typed;
//! anything else is handled as raw JSON through [`crate::Message::content`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Extra = Map<String, Value>;
/// A MIME bundle: mime type to data. Text types are strings; `+json` types
/// are JSON values.
pub type MimeBundle = Map<String, Value>;

// ----- requests the host sends -----------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KernelInfoRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub code: String,
    #[serde(default)]
    pub silent: bool,
    #[serde(default = "default_true")]
    pub store_history: bool,
    #[serde(default)]
    pub user_expressions: Map<String, Value>,
    #[serde(default)]
    pub allow_stdin: bool,
    #[serde(default)]
    pub stop_on_error: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_true() -> bool {
    true
}

impl ExecuteRequest {
    pub fn new(code: impl Into<String>) -> Self {
        ExecuteRequest {
            code: code.into(),
            silent: false,
            store_history: true,
            user_expressions: Map::new(),
            allow_stdin: true,
            stop_on_error: true,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub code: String,
    pub cursor_pos: usize,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectRequest {
    pub code: String,
    pub cursor_pos: usize,
    #[serde(default)]
    pub detail_level: u8,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsCompleteRequest {
    pub code: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShutdownRequest {
    #[serde(default)]
    pub restart: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterruptRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputReply {
    pub value: String,
    #[serde(flatten)]
    pub extra: Extra,
}

// ----- replies --------------------------------------------------------

/// `status` on replies: `ok`, `error`, or `aborted` (queued behind a
/// failed request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyStatus {
    Ok,
    Error,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub mimetype: String,
    #[serde(default)]
    pub file_extension: String,
    /// String or object, per the spec. Left as JSON.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub codemirror_mode: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pygments_lexer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbconvert_exporter: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelInfoReply {
    pub status: ReplyStatus,
    pub protocol_version: String,
    pub implementation: String,
    #[serde(default)]
    pub implementation_version: String,
    pub language_info: LanguageInfo,
    #[serde(default)]
    pub banner: String,
    #[serde(default)]
    pub help_links: Vec<Value>,
    /// Deprecated by `supported_features` (JEP 92) but still sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debugger: Option<bool>,
    /// Protocol 5.5 / JEP 92: optional feature names, e.g. `debugger`,
    /// `kernel subshells`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_features: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl KernelInfoReply {
    pub fn supports(&self, feature: &str) -> bool {
        self.supported_features.iter().any(|f| f == feature)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteReply {
    pub status: ReplyStatus,
    #[serde(default)]
    pub execution_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evalue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceback: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub user_expressions: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShutdownReply {
    pub status: ReplyStatus,
    #[serde(default)]
    pub restart: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

// ----- iopub ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionState {
    Starting,
    Busy,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub execution_state: ExecutionState,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    /// `stdout` or `stderr`.
    pub name: String,
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transient {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayData {
    pub data: MimeBundle,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient: Option<Transient>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteResult {
    pub execution_count: Option<u64>,
    pub data: MimeBundle,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteInput {
    pub code: String,
    pub execution_count: Option<u64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorContent {
    pub ename: String,
    pub evalue: String,
    #[serde(default)]
    pub traceback: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClearOutput {
    #[serde(default)]
    pub wait: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    pub prompt: String,
    #[serde(default)]
    pub password: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

// ----- comms (language-neutral; widgets build on these) -----------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommOpen {
    pub comm_id: String,
    pub target_name: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_module: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommMsg {
    pub comm_id: String,
    #[serde(default)]
    pub data: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommClose {
    pub comm_id: String,
    #[serde(default)]
    pub data: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_info_reply_keeps_extra_fields_and_features() {
        let j = serde_json::json!({
            "status": "ok", "protocol_version": "5.5", "implementation": "cdp-kernel",
            "language_info": {"name": "typescript", "codemirror_mode": "typescript"},
            "supported_features": ["debugger"],
            "capabilities": {"jit": false}
        });
        let r: KernelInfoReply = serde_json::from_value(j).unwrap();
        assert!(r.supports("debugger"));
        assert_eq!(r.extra["capabilities"]["jit"], Value::Bool(false));
        assert_eq!(
            r.language_info.codemirror_mode,
            Value::String("typescript".into())
        );
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["capabilities"]["jit"], Value::Bool(false));
    }

    #[test]
    fn execute_request_defaults() {
        let r: ExecuteRequest = serde_json::from_value(serde_json::json!({"code": "x"})).unwrap();
        assert!(r.store_history);
        assert!(!r.silent);
    }
}
