//! The connection file a host writes and a kernel reads.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Contents of `kernel-<id>.json`. Field names match the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub ip: String,
    /// `tcp` or `ipc`.
    pub transport: String,
    pub shell_port: u16,
    pub iopub_port: u16,
    pub stdin_port: u16,
    pub control_port: u16,
    pub hb_port: u16,
    /// Hex or plain string key; empty means unsigned.
    #[serde(default)]
    pub key: String,
    #[serde(default = "default_scheme")]
    pub signature_scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_scheme() -> String {
    "hmac-sha256".to_string()
}

impl ConnectionInfo {
    /// A new TCP connection file on `ip` with the given ports and a fresh
    /// random key.
    pub fn new_tcp(ip: &str, ports: [u16; 5]) -> Self {
        ConnectionInfo {
            ip: ip.to_string(),
            transport: "tcp".to_string(),
            shell_port: ports[0],
            iopub_port: ports[1],
            stdin_port: ports[2],
            control_port: ports[3],
            hb_port: ports[4],
            key: uuid::Uuid::new_v4().to_string(),
            signature_scheme: default_scheme(),
            kernel_name: None,
            extra: Map::new(),
        }
    }

    /// Endpoint string for one channel, e.g. `tcp://127.0.0.1:5555`.
    pub fn endpoint(&self, port: u16) -> String {
        match self.transport.as_str() {
            "ipc" => format!("ipc://{}-{}", self.ip, port),
            _ => format!("{}://{}:{}", self.transport, self.ip, port),
        }
    }

    pub fn shell_endpoint(&self) -> String {
        self.endpoint(self.shell_port)
    }
    pub fn iopub_endpoint(&self) -> String {
        self.endpoint(self.iopub_port)
    }
    pub fn stdin_endpoint(&self) -> String {
        self.endpoint(self.stdin_port)
    }
    pub fn control_endpoint(&self) -> String {
        self.endpoint(self.control_port)
    }
    pub fn hb_endpoint(&self) -> String {
        self.endpoint(self.hb_port)
    }

    pub fn signer(&self) -> crate::Result<crate::wire::Signer> {
        crate::wire::Signer::new(&self.signature_scheme, self.key.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_jupyter_client_file() {
        let j = r#"{"shell_port": 50000, "iopub_port": 50001, "stdin_port": 50002, "control_port": 50003, "hb_port": 50004, "ip": "127.0.0.1", "key": "abc", "transport": "tcp", "signature_scheme": "hmac-sha256", "kernel_name": "python3"}"#;
        let c: ConnectionInfo = serde_json::from_str(j).unwrap();
        assert_eq!(c.shell_endpoint(), "tcp://127.0.0.1:50000");
        assert_eq!(c.kernel_name.as_deref(), Some("python3"));
    }
}
