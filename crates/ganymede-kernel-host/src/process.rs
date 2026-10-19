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
    /// Where the kernel's ZeroMQ sockets listen. `None` for a kernel with
    /// no sockets, such as one an app runs in-process; its
    /// [`TransportFactory`](crate::TransportFactory) connects it another way.
    pub connection: Option<ConnectionInfo>,
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
    /// Without `quiet`, start each line of that output with
    /// `[kernel <first 8 of kernel_id>] `, for a host running many kernels.
    pub prefix_output: bool,
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
        } else if opts.prefix_output {
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
        } else {
            cmd.stdout(std::io::stderr());
        }
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(false);

        let mut child = spawn_tied_to_host(cmd).await.map_err(|e| {
            let _ = std::fs::remove_file(&connection_file);
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("spawning {:?}: {e}", argv[0]),
            ))
        })?;
        let pid = child.id().ok_or(Error::KernelDied)?;
        tracing::info!(kernel = %spec.name, kernel_id, pid, "launched kernel");
        let prefix = format!("[kernel {}] ", kernel_id.get(..8).unwrap_or(&kernel_id));
        if let Some(out) = child.stdout.take() {
            tokio::spawn(prefix_lines(out, prefix.clone(), write_stderr));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(prefix_lines(err, prefix, write_stderr));
        }

        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(async move {
            let status = child.wait().await.ok();
            let code = status.and_then(|s| s.code());
            tracing::info!(pid, ?code, "kernel exited");
            // Whatever the kernel left in its process group (a background
            // job, a worker it forgot) goes with it, as `jupyter_client`'s
            // `killpg` does on shutdown. Safe so soon after the reap: a
            // group id is not reused as a pid while the group has members,
            // and with none left this finds nothing.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = exit_tx.send(Some(code));
        });

        Ok(LaunchedKernel {
            connection: Some(info),
            connection_file: Some(connection_file),
            process: Arc::new(ChildProcess { pid, exit: exit_rx }),
            interrupt_mode: spec.interrupt_mode,
        })
    }
}

/// Pass `r` to `out` line by line, each line after `prefix` and ending in
/// a newline. The launcher's `out` writes one line at a time to stderr.
#[cfg(feature = "subprocess")]
async fn prefix_lines(
    r: impl tokio::io::AsyncRead + Unpin,
    prefix: String,
    mut out: impl FnMut(&[u8]),
) {
    use tokio::io::AsyncBufReadExt;
    let mut r = tokio::io::BufReader::new(r);
    let mut line = prefix.into_bytes();
    let start = line.len();
    loop {
        line.truncate(start);
        match r.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if !line.ends_with(b"\n") {
                    line.push(b'\n');
                }
                out(&line);
            }
        }
    }
}

#[cfg(feature = "subprocess")]
fn write_stderr(line: &[u8]) {
    use std::io::Write;
    let _ = std::io::stderr().lock().write_all(line);
}

/// Spawn the kernel so it dies with the host, where the OS allows it.
///
/// On Linux the child asks for `SIGKILL` when its parent goes
/// (`PR_SET_PDEATHSIG`), so a host killed with SIGKILL, or one that
/// panics past its cleanup, does not orphan its kernels. Linux sends that
/// signal when the *thread* that forked the child exits, not the process,
/// and a tokio worker or blocking-pool thread can retire while the kernel
/// should live. So every kernel is forked from one dedicated thread that
/// lives as long as the process. Elsewhere this is a plain spawn: kernels
/// that honour `JPY_PARENT_PID` (ipykernel does) still exit on their own.
#[cfg(feature = "subprocess")]
async fn spawn_tied_to_host(
    #[allow(unused_mut)] mut cmd: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id();
        // SAFETY: `prctl`, `getppid` and `raise` are async-signal-safe and
        // the closure allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The host died between fork and prctl: no signal will come.
                if libc::getppid() as u32 != parent {
                    libc::raise(libc::SIGKILL);
                }
                Ok(())
            });
        }
        spawner::spawn(cmd).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        cmd.spawn()
    }
}

#[cfg(all(feature = "subprocess", target_os = "linux"))]
mod spawner {
    use std::sync::mpsc;
    use std::sync::OnceLock;
    use tokio::sync::oneshot;

    type Job = (
        tokio::process::Command,
        tokio::runtime::Handle,
        oneshot::Sender<std::io::Result<tokio::process::Child>>,
    );

    /// The spawner thread's queue. The sender lives in a static, so the
    /// thread's `recv` loop never ends and the thread never exits.
    static JOBS: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

    fn jobs() -> &'static mpsc::Sender<Job> {
        JOBS.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("ganymede-kernel-spawner".into())
                .spawn(move || {
                    for (mut cmd, handle, reply) in rx {
                        // The child's exit is watched by the caller's
                        // runtime, so register it there.
                        let _guard = handle.enter();
                        let _ = reply.send(cmd.spawn());
                    }
                })
                .expect("spawning the kernel spawner thread");
            tx
        })
    }

    pub(super) async fn spawn(
        cmd: tokio::process::Command,
    ) -> std::io::Result<tokio::process::Child> {
        let (tx, rx) = oneshot::channel();
        let gone = || std::io::Error::other("kernel spawner thread is gone");
        jobs()
            .send((cmd, tokio::runtime::Handle::current(), tx))
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
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
    let mut launched = in_process();
    launched.connection = Some(connection);
    launched
}

/// A kernel with no process and no sockets, such as one an app runs on
/// its own thread: what an in-process [`Provisioner`] returns. Its
/// [`TransportFactory`](crate::TransportFactory) supplies the transport,
/// and interrupts go over the control channel.
pub fn in_process() -> LaunchedKernel {
    let (tx, rx) = watch::channel(true);
    let process = NoProcess {
        alive: rx,
        _keep: tx,
    };
    LaunchedKernel {
        connection: None,
        connection_file: None,
        process: Arc::new(process),
        interrupt_mode: InterruptMode::Message,
    }
}

/// Resolve a `--existing` argument as `jupyter_client.find_connection_file`
/// does, minus its globbing: a path that exists is used as given; a bare
/// file name is looked up in `runtime_dir`, as `<name>` and then as
/// `kernel-<name>.json`; an empty path means the most recently modified
/// `kernel-*.json` there.
pub fn find_connection_file(name: &Path, runtime_dir: &Path) -> Result<PathBuf> {
    let not_found =
        |what: String| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, what));
    if name.as_os_str().is_empty() {
        let newest = std::fs::read_dir(runtime_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("kernel-") && n.ends_with(".json")
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .max();
        return newest.map(|(_, p)| p).ok_or_else(|| {
            not_found(format!(
                "no kernel-*.json connection file in {}",
                runtime_dir.display()
            ))
        });
    }
    if name.exists() {
        return Ok(name.to_path_buf());
    }
    let bare = name.components().count() == 1 && name.file_name().is_some();
    if bare {
        let n = name.to_string_lossy();
        for candidate in [
            runtime_dir.join(name),
            runtime_dir.join(format!("kernel-{n}.json")),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err(not_found(format!(
            "no connection file {} here or in {}",
            name.display(),
            runtime_dir.display()
        )));
    }
    Err(not_found(format!("no connection file {}", name.display())))
}

/// Read a connection file written by this host or by `jupyter_client`.
pub fn read_connection_file(path: &Path) -> Result<ConnectionInfo> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[cfg(all(test, feature = "subprocess"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kernel_output_lines_get_the_prefix() {
        let mut got = Vec::new();
        super::prefix_lines(&b"one\ntwo\n\nlast"[..], "[kernel ab] ".into(), |l| {
            got.push(String::from_utf8_lossy(l).into_owned())
        })
        .await;
        assert_eq!(
            got,
            [
                "[kernel ab] one\n",
                "[kernel ab] two\n",
                "[kernel ab] \n",
                "[kernel ab] last\n"
            ]
        );
    }

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

    #[cfg(unix)]
    fn sh_spec(script: &str) -> Kernelspec {
        let mut spec: Kernelspec = serde_json::from_value(serde_json::json!({
            "argv": ["sh", "-c", script],
            "display_name": "",
            "language": "",
        }))
        .unwrap();
        spec.name = "sh".into();
        spec
    }

    #[cfg(unix)]
    async fn launch_sh(runtime_dir: &Path, script: &str) -> LaunchedKernel {
        let prov = SubprocessProvisioner {
            runtime_dir: runtime_dir.into(),
            ip: "127.0.0.1".into(),
        };
        let opts = LaunchOptions {
            quiet: true,
            ..Default::default()
        };
        prov.launch(&sh_spec(script), opts).await.unwrap()
    }

    /// Alive and not a zombie waiting for a reaper.
    #[cfg(unix)]
    fn pid_running(pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return false;
        }
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => !stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
            Err(_) => !cfg!(target_os = "linux"),
        }
    }

    #[cfg(unix)]
    fn read_pid(path: &Path) -> i32 {
        for _ in 0..100 {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("no pid in {}", path.display());
    }

    #[cfg(unix)]
    async fn until_gone(pid: i32) -> bool {
        for _ in 0..100 {
            if !pid_running(pid) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn processes_left_in_the_group_die_with_the_kernel() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bg.pid");
        // A background job stays in the kernel's group; the kernel exits.
        let script = format!("sleep 30 & echo $! > '{}'; exit 0", file.display());
        let k = launch_sh(dir.path(), &script).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), k.process.wait())
            .await
            .unwrap();
        let bg = read_pid(&file);
        assert!(
            until_gone(bg).await,
            "background job {bg} outlived the kernel"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kernel_outlives_the_thread_that_launched_it() {
        // Launch from a thread that then exits, as a tokio worker can.
        let dir = tempfile::tempdir().unwrap();
        let rt_dir = dir.path().to_path_buf();
        let pid = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let k = rt.block_on(launch_sh(&rt_dir, "exec sleep 30"));
            let pid = k.process.pid().unwrap() as i32;
            std::mem::forget(k);
            pid
        })
        .join()
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(pid_running(pid), "kernel died with its launching thread");
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }

    /// Run by [`kernel_dies_with_its_host`] in a child test process: launch
    /// a kernel, report its pid, then wait to be killed.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "helper process for kernel_dies_with_its_host"]
    async fn host_helper() {
        let Some(out) = std::env::var_os("GANYMEDE_TEST_HOST_PID_FILE") else {
            return;
        };
        let out = PathBuf::from(out);
        let k = launch_sh(out.parent().unwrap(), "exec sleep 30").await;
        std::fs::write(&out, k.process.pid().unwrap().to_string()).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn kernel_dies_with_its_host() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("kernel.pid");
        let mut host = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::host_helper", "--ignored"])
            .env("GANYMEDE_TEST_HOST_PID_FILE", &file)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let kernel = read_pid(&file);
        assert!(pid_running(kernel));
        host.kill().unwrap();
        host.wait().unwrap();
        assert!(
            until_gone(kernel).await,
            "kernel {kernel} outlived its host"
        );
    }

    #[test]
    fn connection_files_are_found_in_the_runtime_dir() {
        let rt = tempfile::tempdir().unwrap();
        let info = ConnectionInfo::new_tcp("127.0.0.1", [1, 2, 3, 4, 5]);
        let missing = find_connection_file(Path::new(""), rt.path()).unwrap_err();
        assert!(
            missing.to_string().contains("no kernel-*.json"),
            "{missing}"
        );

        let old = write_connection_file(rt.path(), "old", &info).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(t)
            .unwrap();
        let new = write_connection_file(rt.path(), "new", &info).unwrap();
        std::fs::write(rt.path().join("other.json"), "{}").unwrap();

        let find = |n: &str| find_connection_file(Path::new(n), rt.path());
        assert_eq!(find("").unwrap(), new);
        assert_eq!(find("kernel-old.json").unwrap(), old);
        assert_eq!(find("old").unwrap(), old);
        assert_eq!(find("other.json").unwrap(), rt.path().join("other.json"));
        // A path that exists is used as given.
        assert_eq!(find(old.to_str().unwrap()).unwrap(), old);
        let e = find("nope").unwrap_err();
        assert!(e.to_string().contains("here or in"), "{e}");
        // Only a bare name is searched for.
        assert!(find("sub/kernel-old.json").is_err());
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
