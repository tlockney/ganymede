//! Provisioners: how a kernel comes to exist, and the process handle.

use crate::kernelspec::{InterruptMode, Kernelspec};
use crate::{Error, Result};
use async_trait::async_trait;
use ganymede_protocol::ConnectionInfo;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;

/// A running kernel's process, or whatever stands in for one.
#[async_trait]
pub trait KernelProcess: Send + Sync {
    fn pid(&self) -> Option<u32>;
    /// Deliver an interrupt signal. `Err(Unsupported)` when the process
    /// cannot be signalled; the session then falls back to the control
    /// channel if the kernelspec allows.
    async fn interrupt(&self) -> Result<()>;
    async fn kill(&self) -> Result<()>;
    /// Resolves when the process exits, with its exit code if known.
    async fn wait(&self) -> Option<i32>;
    fn is_alive(&self) -> bool;
}

pub struct LaunchedKernel {
    pub connection: ConnectionInfo,
    /// Written by the provisioner and removed on drop, when applicable.
    pub connection_file: Option<PathBuf>,
    pub process: Arc<dyn KernelProcess>,
    pub interrupt_mode: InterruptMode,
}

impl Drop for LaunchedKernel {
    fn drop(&mut self) {
        if let Some(p) = &self.connection_file {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub kernel_id: Option<String>,
}

#[async_trait]
pub trait Provisioner: Send + Sync {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel>;
}

// ----- subprocess --------------------------------------------------------

/// Launches a kernel as a child process from its kernelspec.
#[derive(Debug, Clone)]
pub struct SubprocessProvisioner {
    pub runtime_dir: PathBuf,
    pub ip: String,
}

impl Default for SubprocessProvisioner {
    fn default() -> Self {
        SubprocessProvisioner {
            runtime_dir: crate::kernelspec::jupyter_runtime_dir(),
            ip: "127.0.0.1".into(),
        }
    }
}

/// Five free TCP ports on `ip`. Racy by nature: the ports are released
/// before the kernel binds them. `ipc` transport avoids this on POSIX.
pub async fn allocate_ports(ip: &str) -> Result<[u16; 5]> {
    let mut listeners = Vec::new();
    let mut ports = [0u16; 5];
    for p in &mut ports {
        let l = tokio::net::TcpListener::bind((ip, 0)).await?;
        *p = l.local_addr()?.port();
        listeners.push(l);
    }
    drop(listeners);
    Ok(ports)
}

pub fn write_connection_file(
    dir: &Path,
    kernel_id: &str,
    info: &ConnectionInfo,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("kernel-{kernel_id}.json"));
    let text = serde_json::to_string_pretty(info)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

struct ChildProcess {
    pid: u32,
    exit: watch::Receiver<Option<Option<i32>>>,
}

#[async_trait]
impl KernelProcess for ChildProcess {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    async fn interrupt(&self) -> Result<()> {
        #[cfg(unix)]
        {
            // The kernel is its own process group leader; signal the group so
            // children it spawned get it too.
            let rc = unsafe { libc::kill(-(self.pid as i32), libc::SIGINT) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported("signal interrupt on this platform"))
        }
    }

    async fn kill(&self) -> Result<()> {
        #[cfg(unix)]
        {
            let rc = unsafe { libc::kill(-(self.pid as i32), libc::SIGKILL) };
            if rc != 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::ESRCH) {
                    return Err(e.into());
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported("kill on this platform"))
        }
    }

    async fn wait(&self) -> Option<i32> {
        let mut rx = self.exit.clone();
        loop {
            if let Some(code) = *rx.borrow() {
                return code;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    fn is_alive(&self) -> bool {
        self.exit.borrow().is_none()
    }
}

#[async_trait]
impl Provisioner for SubprocessProvisioner {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel> {
        let kernel_id = opts
            .kernel_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let ports = allocate_ports(&self.ip).await?;
        let mut info = ConnectionInfo::new_tcp(&self.ip, ports);
        info.kernel_name = Some(spec.name.clone());
        let connection_file = write_connection_file(&self.runtime_dir, &kernel_id, &info)?;

        let argv = spec.command(&connection_file);
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        // The kernel inherits the host's environment, then the kernelspec's
        // `env`, then per-launch overrides.
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        cmd.env("JPY_PARENT_PID", std::process::id().to_string());
        cmd.env("JPY_SESSION_NAME", &kernel_id);
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::null());
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(false);

        let mut child = cmd.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&connection_file);
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("spawning {:?}: {e}", argv[0]),
            ))
        })?;
        let pid = child.id().ok_or(Error::KernelDied)?;
        tracing::info!(kernel = %spec.name, pid, "launched kernel");

        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(async move {
            let status = child.wait().await.ok();
            let code = status.and_then(|s| s.code());
            tracing::info!(pid, ?code, "kernel exited");
            let _ = exit_tx.send(Some(code));
        });

        Ok(LaunchedKernel {
            connection: info,
            connection_file: Some(connection_file),
            process: Arc::new(ChildProcess { pid, exit: exit_rx }),
            interrupt_mode: spec.interrupt_mode,
        })
    }
}

// ----- attach --------------------------------------------------------------

/// A kernel someone else started. No process handle, so interrupts go
/// over the control channel and `wait` never resolves.
struct NoProcess {
    alive: watch::Receiver<bool>,
    /// Held so `alive` never reports closed; `wait` pends forever.
    _keep: watch::Sender<bool>,
}

#[async_trait]
impl KernelProcess for NoProcess {
    fn pid(&self) -> Option<u32> {
        None
    }
    async fn interrupt(&self) -> Result<()> {
        Err(Error::Unsupported(
            "no process handle; use interrupt_mode=message",
        ))
    }
    async fn kill(&self) -> Result<()> {
        Err(Error::Unsupported("no process handle"))
    }
    async fn wait(&self) -> Option<i32> {
        let mut rx = self.alive.clone();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        None
    }
    fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }
}

/// Use an already running kernel from its connection info.
pub fn attach(connection: ConnectionInfo) -> LaunchedKernel {
    let (tx, rx) = watch::channel(true);
    let process = NoProcess {
        alive: rx,
        _keep: tx,
    };
    LaunchedKernel {
        connection,
        connection_file: None,
        process: Arc::new(process),
        interrupt_mode: InterruptMode::Message,
    }
}

/// Read a connection file written by this host or by `jupyter_client`.
pub fn read_connection_file(path: &Path) -> Result<ConnectionInfo> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ports_are_distinct_and_connection_file_is_private() {
        let ports = allocate_ports("127.0.0.1").await.unwrap();
        let set: std::collections::HashSet<_> = ports.iter().collect();
        assert_eq!(set.len(), 5);
        let dir = tempfile::tempdir().unwrap();
        let info = ConnectionInfo::new_tcp("127.0.0.1", ports);
        let path = write_connection_file(dir.path(), "abc", &info).unwrap();
        assert!(path.ends_with("kernel-abc.json"));
        let back = read_connection_file(&path).unwrap();
        assert_eq!(back, info);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
