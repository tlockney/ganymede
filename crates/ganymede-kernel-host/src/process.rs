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
    /// Ask the process to exit (SIGTERM on Unix), the step between a
    /// `shutdown_request` and [`KernelProcess::kill`]. `Err(Unsupported)`
    /// by default; the session then goes straight to `kill`.
    async fn terminate(&self) -> Result<()> {
        Err(Error::Unsupported("terminate"))
    }
    async fn kill(&self) -> Result<()>;
    /// Resolves when the process exits, with its exit code if known.
    async fn wait(&self) -> Option<i32>;
    fn is_alive(&self) -> bool;
    /// Kill without waiting, for use in `Drop`. Does nothing by default.
    fn kill_now(&self) {}
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
    /// Discard the kernel process's own stdout and stderr (not its cell
    /// output, which travels over iopub). Otherwise both go to the host's
    /// stderr, so a host's stdout stays free for data.
    pub quiet: bool,
}

#[async_trait]
pub trait Provisioner: Send + Sync {
    async fn launch(&self, spec: &Kernelspec, opts: LaunchOptions) -> Result<LaunchedKernel>;
}

// ----- subprocess --------------------------------------------------------

#[cfg(feature = "subprocess")]
/// Launches a kernel as a child process from its kernelspec.
#[derive(Debug, Clone)]
pub struct SubprocessProvisioner {
    pub runtime_dir: PathBuf,
    pub ip: String,
}

#[cfg(feature = "subprocess")]
impl Default for SubprocessProvisioner {
    fn default() -> Self {
        SubprocessProvisioner {
            runtime_dir: crate::kernelspec::jupyter_runtime_dir(),
            ip: "127.0.0.1".into(),
        }
    }
}

#[cfg(feature = "subprocess")]
/// Five free TCP ports on `ip`, picked at random below the ephemeral port
/// ranges (Linux 32768-60999, macOS and Windows 49152-65535). Port 0 would
/// return an ephemeral port, which outgoing connections also take their
/// source ports from: while the kernel starts, a client reconnecting to
/// that port can be given it as its own source port and connect to
/// itself, and the kernel then fails to bind. Racy against other
/// processes binding the same port; `ipc` transport avoids this on POSIX.
pub async fn allocate_ports(ip: &str) -> Result<[u16; 5]> {
    const RANGE: std::ops::Range<u16> = 20000..32768;
    let mut listeners = Vec::new();
    let mut ports = [0u16; 5];
    let mut found = 0;
    for _ in 0..1000 {
        if found == ports.len() {
            break;
        }
        let span = u128::from(RANGE.end - RANGE.start);
        let port = RANGE.start + (uuid::Uuid::new_v4().as_u128() % span) as u16;
        if ports[..found].contains(&port) {
            continue;
        }
        if let Ok(l) = tokio::net::TcpListener::bind((ip, port)).await {
            ports[found] = port;
            listeners.push(l);
            found += 1;
        }
    }
    // A crowded range: fall back to ports the OS picks.
    for p in &mut ports[found..] {
        let l = tokio::net::TcpListener::bind((ip, 0)).await?;
        *p = l.local_addr()?.port();
        listeners.push(l);
    }
    drop(listeners);
    Ok(ports)
}

/// Writes `kernel-<kernel_id>.json` in `dir`, readable only by this user
/// from the moment it exists, as `jupyter_core`'s `secure_write` does: any
/// old file is removed, then the new one is created exclusively with mode
/// 0600, so a pre-planted file or symlink is never written through. A
/// `dir` this call creates is made 0700, like Jupyter's runtime dir.
pub fn write_connection_file(
    dir: &Path,
    kernel_id: &str,
    info: &ConnectionInfo,
) -> Result<PathBuf> {
    if !is_file_name_token(kernel_id) {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("kernel id {kernel_id:?} is not a valid file name"),
        )));
    }
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let path = dir.join(format!("kernel-{kernel_id}.json"));
    let text = serde_json::to_string_pretty(info)?;
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    opts.open(&path)?.write_all(text.as_bytes())?;
    Ok(path)
}

/// Letters, digits, `-` and `_`: what a UUID or a session name needs, and
/// nothing that can leave the directory or hide the file.
fn is_file_name_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(feature = "subprocess")]
struct ChildProcess {
    pid: u32,
    exit: watch::Receiver<Option<Option<i32>>>,
}

#[cfg(feature = "subprocess")]
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
            self.signal_group(libc::SIGINT)
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported("signal interrupt on this platform"))
        }
    }

    async fn terminate(&self) -> Result<()> {
        #[cfg(unix)]
        {
            match self.signal_group(libc::SIGTERM) {
                Err(Error::KernelDied) => Ok(()),
                other => other,
            }
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported("terminate on this platform"))
        }
    }

    async fn kill(&self) -> Result<()> {
        #[cfg(unix)]
        {
            match self.signal_group(libc::SIGKILL) {
                Err(Error::KernelDied) => Ok(()),
                other => other,
            }
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

    fn kill_now(&self) {
        #[cfg(unix)]
        let _ = self.signal_group(libc::SIGKILL);
    }
}

#[cfg(all(feature = "subprocess", unix))]
impl ChildProcess {
    /// Signal the kernel's process group, unless the kernel has already
    /// been reaped: its pid, and so the group id, may then belong to an
    /// unrelated process. `Err(KernelDied)` in that case. A narrow race
    /// remains between this check and the signal; the waiter task reaps
    /// as soon as the child exits.
    fn signal_group(&self, sig: libc::c_int) -> Result<()> {
        if !self.is_alive() {
            return Err(Error::KernelDied);
        }
        let rc = unsafe { libc::kill(-(self.pid as i32), sig) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ESRCH) {
                return Err(Error::KernelDied);
            }
            return Err(e.into());
        }
        Ok(())
    }
}

#[cfg(feature = "subprocess")]
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
        if opts.quiet {
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        } else {
            cmd.stdout(std::io::stderr());
        }
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

#[cfg(all(test, feature = "subprocess"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ports_are_distinct_and_connection_file_is_private() {
        let ports = allocate_ports("127.0.0.1").await.unwrap();
        let set: std::collections::HashSet<_> = ports.iter().collect();
        assert_eq!(set.len(), 5);
        assert!(
            ports.iter().all(|p| (20000..32768).contains(p)),
            "{ports:?}"
        );
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

    #[test]
    fn connection_file_is_created_private_and_replaces_an_old_one() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("runtime");
        let info = ConnectionInfo::new_tcp("127.0.0.1", [1, 2, 3, 4, 5]);
        let path = write_connection_file(&dir, "k1", &info).unwrap();
        // A restart writes the same name again.
        let mut again = info.clone();
        again.shell_port = 9;
        write_connection_file(&dir, "k1", &again).unwrap();
        assert_eq!(read_connection_file(&path).unwrap(), again);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700, "a runtime dir the host creates");
            assert_eq!(mode(&path), 0o600);

            // A planted symlink is replaced, not written through.
            let target = root.path().join("target");
            std::fs::write(&target, "untouched").unwrap();
            let planted = dir.join("kernel-k2.json");
            std::os::unix::fs::symlink(&target, &planted).unwrap();
            write_connection_file(&dir, "k2", &info).unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
            assert!(!std::fs::symlink_metadata(&planted).unwrap().is_symlink());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reaped_kernel_is_not_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec: Kernelspec =
            serde_json::from_str(r#"{"argv":["sleep","30"],"display_name":"","language":""}"#)
                .unwrap();
        spec.name = "sleep".into();
        let prov = SubprocessProvisioner {
            runtime_dir: dir.path().into(),
            ip: "127.0.0.1".into(),
        };
        let k = prov
            .launch(
                &spec,
                LaunchOptions {
                    quiet: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(k.process.is_alive());
        k.process.terminate().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), k.process.wait())
            .await
            .unwrap();
        assert!(!k.process.is_alive());
        assert!(matches!(
            k.process.interrupt().await,
            Err(Error::KernelDied)
        ));
        k.process.terminate().await.unwrap();
        k.process.kill().await.unwrap();
    }

    #[test]
    fn kernel_ids_that_are_not_file_names_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let info = ConnectionInfo::new_tcp("127.0.0.1", [1, 2, 3, 4, 5]);
        for bad in [
            "",
            "../x",
            "a/b",
            ".hidden",
            "a\\b",
            "a\0b",
            &"x".repeat(129),
        ] {
            assert!(
                write_connection_file(dir.path(), bad, &info).is_err(),
                "{bad:?}"
            );
        }
        write_connection_file(dir.path(), "6f1c0c2e-2b9f-4c1e-9d8b-1a2b3c4d5e6f", &info).unwrap();
    }
}
