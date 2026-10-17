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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecModel {
    pub name: String,
    pub spec: KernelspecSpec,
    /// Logo and other resource URLs. Empty in this sketch.
    pub resources: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KernelspecsModel {
    /// Clients use this when creating a notebook with no kernel chosen.
    /// There is no built-in value; see [`crate::KernelManagerConfig`].
    pub default: String,
    pub kernelspecs: BTreeMap<String, KernelspecModel>,
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
