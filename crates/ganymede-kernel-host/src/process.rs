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
/// How long a port handed out by [`allocate_ports`] is kept from being
/// handed out again: long enough for its kernel to bind it, including
/// cdp-kernel's retries.
const PORT_RESERVATION: std::time::Duration = std::time::Duration::from_secs(300);

#[cfg(feature = "subprocess")]
/// Ports [`allocate_ports`] handed out recently, with when. A port is free
/// again once its listener is dropped, so without this two kernels
/// started at once by this process can be given the same port before
/// either binds it. One then fails to bind: ipykernel exits before
/// answering, but cdp-kernel retries for a few seconds, answers
/// `kernel_info` meanwhile, and dies after the start has succeeded.
/// jupyter_client's `LocalPortCache` does the same job.
static RESERVED_PORTS: std::sync::Mutex<std::collections::BTreeMap<u16, std::time::Instant>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(feature = "subprocess")]
/// Reserve `port` unless this process handed it out recently.
fn reserve_port(port: u16) -> bool {
    let now = std::time::Instant::now();
    let mut reserved = RESERVED_PORTS.lock().unwrap();
    reserved.retain(|_, at| now.duration_since(*at) < PORT_RESERVATION);
    if reserved.contains_key(&port) {
        return false;
    }
    reserved.insert(port, now);
    true
}

#[cfg(feature = "subprocess")]
/// Five free TCP ports on `ip`, picked at random below the ephemeral port
/// ranges (Linux 32768-60999, macOS and Windows 49152-65535). Port 0 would
/// return an ephemeral port, which outgoing connections also take their
/// source ports from: while the kernel starts, a client reconnecting to
/// that port can be given it as its own source port and connect to
/// itself, and the kernel then fails to bind. No port is handed out twice
/// within five minutes, so kernels this process starts at once
/// never share one. Still racy against other processes binding the same
/// port; `ipc` transport avoids this on POSIX.
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
            if reserve_port(port) {
                ports[found] = port;
                listeners.push(l);
                found += 1;
            }
        }
    }
    // A crowded range: fall back to ports the OS picks. Holding each
    // listener keeps the OS from returning the same port twice here.
    while found < ports.len() {
        let l = tokio::net::TcpListener::bind((ip, 0)).await?;
        let port = l.local_addr()?.port();
        listeners.push(l);
        if reserve_port(port) {
            ports[found] = port;
            found += 1;
        }
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
    /// The job holding the kernel and everything it starts. `None` if the
    /// job could not be set up; the kernel then runs without one.
    #[cfg(windows)]
    job: Option<Arc<job::Job>>,
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
        #[cfg(windows)]
        {
            self.terminate_job()
        }
        #[cfg(not(any(unix, windows)))]
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
        #[cfg(windows)]
        let _ = self.terminate_job();
    }
}

#[cfg(all(feature = "subprocess", windows))]
impl ChildProcess {
    /// Kill the kernel and every process in its job, the counterpart of
    /// SIGKILL to the process group. Without a job, only `Unsupported`.
    fn terminate_job(&self) -> Result<()> {
        if !self.is_alive() {
            return Ok(());
        }
        match &self.job {
            Some(job) => Ok(job.terminate()?),
            None => Err(Error::Unsupported("kill without a job object")),
        }
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
        // Started suspended, so it can't start a child before it is in
        // its job; resumed below.
        #[cfg(windows)]
        cmd.creation_flags(job::CREATE_SUSPENDED);
        cmd.kill_on_drop(false);

        let mut child = spawn_tied_to_host(cmd).await.map_err(|e| {
            let _ = std::fs::remove_file(&connection_file);
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("spawning {:?}: {e}", argv[0]),
            ))
        })?;
        let pid = child.id().ok_or(Error::KernelDied)?;
        #[cfg(windows)]
        let job = match job::adopt(&child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.start_kill();
                let _ = std::fs::remove_file(&connection_file);
                return Err(Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("resuming {:?}: {e}", argv[0]),
                )));
            }
        };
        tracing::info!(kernel = %spec.name, kernel_id, pid, "launched kernel");
        let prefix = format!("[kernel {}] ", kernel_id.get(..8).unwrap_or(&kernel_id));
        if let Some(out) = child.stdout.take() {
            tokio::spawn(prefix_lines(out, prefix.clone(), write_stderr));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(prefix_lines(err, prefix, write_stderr));
        }

        let (exit_tx, exit_rx) = watch::channel(None);
        #[cfg(windows)]
        let leftovers = job.clone();
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
            // The same for what is left in its job.
            #[cfg(windows)]
            if let Some(job) = leftovers {
                let _ = job.terminate();
            }
            let _ = exit_tx.send(Some(code));
        });

        Ok(LaunchedKernel {
            connection: Some(info),
            connection_file: Some(connection_file),
            process: Arc::new(ChildProcess {
                pid,
                exit: exit_rx,
                #[cfg(windows)]
                job,
            }),
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

/// Windows job objects: the counterpart of the Unix process group. A
/// kernel runs in its own job, so killing the job kills every process the
/// kernel started, and the job is set to kill them all when its last
/// handle closes, which happens at the latest when the host exits.
///
/// Public so other process launchers in the host (language servers)
/// share it: start the process with [`job::CREATE_SUSPENDED`], then
/// [`job::adopt`] it.
#[cfg(all(feature = "subprocess", windows))]
pub mod job {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    /// The creation flag [`adopt`] expects the process to be started with,
    /// so it can't start a child before it is in its job.
    pub const CREATE_SUSPENDED: u32 = windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    /// A job object; every process in it dies when it is terminated or
    /// when its last handle closes.
    pub struct Job(OwnedHandle);

    impl Job {
        /// A job whose processes all die when its last handle closes.
        fn new() -> io::Result<Job> {
            // SAFETY: null attributes and name are allowed; the handle is
            // checked before it is owned.
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(unsafe { OwnedHandle::from_raw_handle(h) });
            // SAFETY: an all-zero struct is a valid "no limits" value.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `info` is the struct this information class takes,
            // with its size.
            let ok = unsafe {
                SetInformationJobObject(
                    job.raw(),
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        fn raw(&self) -> HANDLE {
            self.0.as_raw_handle() as HANDLE
        }

        fn assign(&self, process: HANDLE) -> io::Result<()> {
            // SAFETY: both handles are open for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.raw(), process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Kill every process in the job.
        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: the job handle is open.
            if unsafe { TerminateJobObject(self.raw(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// Put a process started with [`CREATE_SUSPENDED`] in a new job, then
    /// resume it. It is resumed even when the job can't be set up (the
    /// host may itself be in a job that forbids it), so it runs either
    /// way; it then just isn't killed with its children, and this returns
    /// `Ok(None)`. An error means the process could not be resumed and
    /// stays suspended: the caller should kill it.
    pub fn adopt(child: &tokio::process::Child) -> io::Result<Option<Arc<Job>>> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the process has already exited"))?;
        let job = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("no process handle"))
            .and_then(|process| {
                let job = Job::new()?;
                job.assign(process as HANDLE)?;
                Ok(job)
            });
        resume(pid)?;
        match job {
            Ok(job) => Ok(Some(Arc::new(job))),
            Err(e) => {
                tracing::warn!(pid, "process runs without a job object: {e}");
                Ok(None)
            }
        }
    }

    /// Resume the threads of a process started suspended: its main
    /// thread, the only one it has. `std` doesn't hand out the thread
    /// handle, so find it in a snapshot of the system's threads.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: plain call; the handle is checked before it is owned.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snap = unsafe { OwnedHandle::from_raw_handle(snap) };
        let snap = snap.as_raw_handle() as HANDLE;
        // SAFETY: zeroed is valid; `dwSize` must be set before the call.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0;
        // SAFETY: `entry` is a valid THREADENTRY32 with its size set.
        let mut more = unsafe { Thread32First(snap, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the thread handle is checked, used once, and
                // closed by `OwnedHandle`.
                let t = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if !t.is_null() {
                    let t = unsafe { OwnedHandle::from_raw_handle(t) };
                    if unsafe { ResumeThread(t.as_raw_handle() as HANDLE) } != u32::MAX {
                        resumed += 1;
                    }
                }
            }
            more = unsafe { Thread32Next(snap, &mut entry) } != 0;
        }
        if resumed == 0 {
            return Err(io::Error::other(
                "no thread of the process could be resumed",
            ));
        }
        Ok(())
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

    /// Kernels started at once must not share a port (#260): each port is
    /// free once `allocate_ports` returns, so only the reservation keeps
    /// the next call from picking it again.
    #[tokio::test]
    async fn concurrent_allocations_never_share_a_port() {
        let calls = (0..400).map(|_| tokio::spawn(allocate_ports("127.0.0.1")));
        let mut seen = std::collections::HashSet::new();
        for call in futures::future::join_all(calls).await {
            for port in call.unwrap().unwrap() {
                assert!(seen.insert(port), "port {port} handed out twice");
            }
        }
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
