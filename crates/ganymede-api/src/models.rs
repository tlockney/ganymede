//! JSON models, field for field as `jupyter_server` returns them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelModel {
    pub id: String,
    pub name: String,
    pub last_activity: String,
    /// `starting`, `busy`, `idle`, `restarting`, or `dead`.
    pub execution_state: String,
    pub connections: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecSpec {
    pub argv: Vec<String>,
    pub display_name: String,
    pub language: String,
    pub interrupt_mode: String,
    pub env: BTreeMap<String, String>,
    pub metadata: Map<String, Value>,
    /// From `kernel.json`, `""` when absent, as `jupyter_client` 8 reports it.
    #[serde(default)]
    pub kernel_protocol_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecModel {
    pub name: String,
    pub spec: KernelspecSpec,
    /// Logo and other resource URLs, as `jupyter_server` reports them.
    pub resources: Map<String, Value>,
    /// Absolute path of the directory holding the kernelspec's
    /// `kernel.json` and resource files. An extra key, not in
    /// `jupyter_server`'s model. Left out when the spec has no directory:
    /// a gateway spec or a kernel an app registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_dir: Option<String>,
    /// The kernels run on the gateway, not on this machine. An extra key,
    /// not in `jupyter_server`'s model, left out of local specs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gateway: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// What `/api/kernelspecs` reports about the gateway this server runs
/// kernels on, besides the kernelspecs in the same answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayModel {
    /// `gateway_name`, or else the gateway URL's host: never its path,
    /// query or token.
    pub name: String,
    /// Whether the last refresh reached it. Kernelspecs go stale, and
    /// starting a kernel fails, while it is false.
    pub reachable: bool,
    /// Why the last refresh failed; none while reachable.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecsModel {
    /// Clients use this when creating a notebook with no kernel chosen.
    /// There is no built-in value; see [`crate::KernelManagerConfig`].
    pub default: String,
    pub kernelspecs: BTreeMap<String, KernelspecModel>,
    /// The gateway's status, when a gateway is configured. An extra key,
    /// not in `jupyter_server`'s model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotebookRef {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionModel {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub kernel: KernelModel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notebook: Option<NotebookRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusModel {
    pub started: String,
    pub last_activity: String,
    pub connections: usize,
    pub kernels: usize,
}

/// `POST /api/kernels` body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KernelStart {
    #[serde(default)]
    pub name: Option<String>,
    /// API path of the notebook; the kernel's working directory is its
    /// directory under the contents root.
    #[serde(default)]
    pub path: Option<String>,
}
