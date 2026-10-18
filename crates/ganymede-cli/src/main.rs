//! `ganymede`: the command line for the engine. Every subcommand is a thin
//! layer over `ganymede-engine` (or `ganymede-server` for `serve`).

mod console;
mod lab;
mod lab_start;
mod render;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use ganymede_engine::host::in_process_pair;
use ganymede_engine::host::kernelspec as ks;
use ganymede_engine::host::testing::{FakeKernel, ReplyOrder};
use ganymede_engine::nbformat::{Cell, Notebook};
use ganymede_engine::{Engine, ExecuteOptions, Kernel, StartOptions};
use render::Terminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "ganymede",
    version,
    about = "Language-agnostic Jupyter engine: kernels, notebooks, and a server",
    after_help = "The kernel name `fake` is a built-in in-process kernel for trying things out."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Extra kernelspec roots (directories holding `kernels/`), searched
    /// before the Jupyter data dirs. Repeatable.
    #[arg(long, global = true, value_name = "DIR")]
    kernelspec_root: Vec<PathBuf>,
    /// Discard kernel processes' own stdout and stderr (not cell output).
    #[arg(long, short, global = true)]
    quiet: bool,
    /// Report kernel details and per-cell status on stderr. For `serve`
    /// and `lab`, log at debug level.
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Seconds a new kernel has to answer. Default 60, or for `serve` and
    /// `lab`, `startup_timeout_secs` from the config file.
    #[arg(long, global = true, value_name = "SECS")]
    startup_timeout: Option<u64>,
}

impl Cli {
    /// Whether this runs a server, which logs at info level by default as
    /// the `ganymede-server` binary does.
    fn serves(&self) -> bool {
        matches!(self.cmd, Cmd::Serve(_) | Cmd::Lab { cmd: None, .. })
    }

    /// The default log filter when `RUST_LOG` is unset.
    fn log_filter(&self) -> &'static str {
        match (self.serves(), self.verbose) {
            (true, false) => "info,zeromq=error",
            (true, true) => "debug,zeromq=error,hyper=info,h2=info",
            (false, _) => "warn,zeromq=error",
        }
    }
}

/// A server's config from its flags, with the global flags that apply
/// to it on top: kernelspec roots, `--startup-timeout` and `-q`.
fn server_config(
    args: ganymede_server::serve::ServeArgs,
    kernelspec_roots: &[PathBuf],
    startup_timeout: Option<u64>,
    quiet: bool,
) -> Result<ganymede_server::config::Config> {
    let mut cfg = args.into_config()?;
    // `serve`'s own `--kernelspec-root` shares the global's name, so clap
    // can hand the same roots to both.
    for r in kernelspec_roots {
        if !cfg.kernelspec_roots.contains(r) {
            cfg.kernelspec_roots.push(r.clone());
        }
    }
    if let Some(t) = startup_timeout {
        cfg.startup_timeout_secs = t;
    }
    cfg.quiet |= quiet;
    Ok(cfg)
}

#[derive(Subcommand)]
enum Cmd {
    /// List, install, and remove kernelspecs.
    Kernelspec {
        #[command(subcommand)]
        cmd: KernelspecCmd,
    },
    /// Run code on a kernel and print what it outputs.
    ///
    /// Each CODE argument and each --file is one cell, run in order. With
    /// neither, stdin is read as one cell. Exits 1 if a cell fails.
    Run(RunArgs),
    /// Interactive console on a kernel. Reads stdin as a script when it is
    /// not a terminal.
    Console {
        #[command(flatten)]
        target: Target,
    },
    /// Execute notebooks as `jupyter execute` (nbclient) does.
    ///
    /// Outputs, execution counts, timings, and `language_info` are written
    /// as nbclient writes them. Without --output or --inplace the result
    /// is not saved, which checks that the notebook runs.
    #[command(alias = "exec")]
    Execute(ExecuteArgs),
    /// Start a kernel and keep it running until Ctrl-C, for other clients
    /// to attach to by its connection file (`jupyter kernel`).
    Kernel {
        #[arg(long, short)]
        kernel: String,
        /// Working directory for the kernel.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Run code on a kernel that is already running (`run --existing`).
    #[command(hide = true)]
    Attach {
        connection_file: PathBuf,
        code: Vec<String>,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Notebook file utilities that need no kernel.
    Nb {
        #[command(subcommand)]
        cmd: NbCmd,
    },
    /// Trust notebooks' HTML and JavaScript output, as `jupyter trust` does.
    ///
    /// Records each notebook's signature with the key and database Jupyter
    /// uses (`notebook_secret` and `nbsignatures.db` in the Jupyter data
    /// dir), so the server and Jupyter render its outputs unsanitized. With
    /// no NOTEBOOK, one is read from stdin.
    Trust {
        notebooks: Vec<PathBuf>,
        /// Delete the trusted signatures and generate a new key. Every
        /// notebook becomes untrusted.
        #[arg(long)]
        reset: bool,
        /// Directory with the key and signatures instead of the Jupyter
        /// data dir.
        #[arg(long, value_name = "DIR")]
        notary_dir: Option<PathBuf>,
    },
    /// Serve JupyterLab in the current directory and open it in the
    /// browser; or install, list, and remove JupyterLab's frontend.
    #[command(args_conflicts_with_subcommands = true)]
    Lab {
        #[command(subcommand)]
        cmd: Option<lab::LabCmd>,
        #[command(flatten)]
        start: Box<lab_start::StartArgs>,
    },
    /// Serve the Jupyter REST and websocket API (JupyterLab, VS Code, ...).
    Serve(Box<ganymede_server::serve::ServeArgs>),
    /// Show versions and the directories in use.
    Info {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum KernelspecCmd {
    /// List installed kernelspecs.
    List {
        /// JSON in `jupyter kernelspec list --json`'s shape.
        #[arg(long)]
        json: bool,
    },
    /// Print the directories searched, in order.
    Paths,
    /// Install a kernelspec directory (one holding `kernel.json`).
    Install {
        source: PathBuf,
        /// Name to install as. Defaults to the directory's name.
        #[arg(long)]
        name: Option<String>,
        /// Install under PREFIX/share/jupyter instead of the user data dir.
        #[arg(long)]
        prefix: Option<PathBuf>,
        /// Overwrite an existing kernelspec of the same name.
        #[arg(long)]
        replace: bool,
    },
    /// Remove kernelspecs by name.
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

#[derive(Subcommand)]
enum NbCmd {
    /// Remove outputs, execution counts, and execution timings.
    Clear {
        notebook: PathBuf,
        #[command(flatten)]
        dest: Dest,
    },
}

/// Which kernel to talk to: a kernelspec to launch, or a running kernel.
#[derive(Args, Clone)]
struct Target {
    /// Kernelspec name, or `fake`.
    #[arg(long, short, required_unless_present = "existing")]
    kernel: Option<String>,
    /// Connection file of a running kernel instead of launching one. A
    /// bare name such as `kernel-<id>.json` or `<id>` is also looked for
    /// in the Jupyter runtime dir; with no value, the newest kernel there.
    #[arg(
        long,
        conflicts_with = "kernel",
        value_name = "CONNECTION_FILE",
        num_args = 0..=1,
        default_missing_value = "",
        // Allows the empty value that means "newest".
        value_parser = clap::builder::TypedValueParser::map(
            clap::builder::OsStringValueParser::new(),
            PathBuf::from
        )
    )]
    existing: Option<PathBuf>,
    /// Working directory for a launched kernel.
    #[arg(long)]
    cwd: Option<PathBuf>,
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    target: Target,
    /// Code to run; one cell per argument.
    code: Vec<String>,
    /// File whose contents are one cell. Repeatable; runs after CODE.
    #[arg(long, short)]
    file: Vec<PathBuf>,
    /// Print one JSON object per cell (status, count, nbformat outputs)
    /// instead of rendering output.
    #[arg(long)]
    json: bool,
    /// Seconds to wait for each cell. 0 waits forever.
    #[arg(long, default_value_t = 0)]
    timeout: u64,
    /// Interrupt each cell after this many milliseconds.
    #[arg(long, hide = true)]
    interrupt_after_ms: Option<u64>,
    /// Keep running cells after one fails.
    #[arg(long)]
    keep_going: bool,
}

#[derive(Args)]
struct ExecuteArgs {
    #[arg(required = true)]
    notebooks: Vec<PathBuf>,
    #[command(flatten)]
    dest: Dest,
    /// Kernelspec name. Defaults to each notebook's
    /// `metadata.kernelspec.name`.
    #[arg(long, short)]
    kernel: Option<String>,
    /// Record errors and keep going.
    #[arg(long)]
    allow_errors: bool,
    /// Error names that do not stop execution. Repeatable.
    #[arg(long, value_name = "ENAME")]
    allow_error_name: Vec<String>,
    /// Stop on every error, overriding the allowances and
    /// `raises-exception` tags.
    #[arg(long)]
    force_raise_errors: bool,
    /// Seconds per cell. 0 waits forever.
    #[arg(long, default_value_t = 0)]
    timeout: u64,
    /// Cells with this tag are skipped.
    #[arg(long, default_value = "skip-execution")]
    skip_tag: String,
    /// Merge each cell's streams and resolve carriage returns.
    #[arg(long)]
    coalesce_streams: bool,
    /// Do not record `metadata.execution` timings.
    #[arg(long)]
    no_timing: bool,
    /// Do not save widget state in `metadata.widgets`.
    #[arg(long)]
    no_widget_state: bool,
}

#[derive(Args, Clone)]
struct Dest {
    /// Where to write the result. `-` is stdout. `{notebook_name}` is
    /// replaced by the input's name without extension.
    #[arg(long, short, conflicts_with = "inplace")]
    output: Option<String>,
    /// Overwrite the input.
    #[arg(long)]
    inplace: bool,
}

impl Dest {
    /// `None`: do not save. `Some(None)`: stdout.
    fn resolve(&self, input: &Path) -> Option<Option<PathBuf>> {
        if self.inplace {
            return Some(Some(input.to_path_buf()));
        }
        let out = self.output.as_ref()?;
        if out == "-" {
            return Some(None);
        }
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(Some(PathBuf::from(out.replace("{notebook_name}", &stem))))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_filter().into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ganymede: {}", error_chain(&e));
            ExitCode::FAILURE
        }
    }
}

/// The error and its causes joined by `: `, skipping a cause whose text
/// its parent already includes (many errors print their source in their
/// own message, so `{:#}` would show it twice).
fn error_chain(e: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in e.chain() {
        let s = cause.to_string();
        if parts.last().is_some_and(|prev| prev.contains(&s)) {
            continue;
        }
        parts.push(s);
    }
    parts.join(": ")
}

async fn dispatch(cli: Cli) -> Result<ExitCode> {
    let mut b = Engine::builder()
        .quiet(cli.quiet)
        .startup_timeout(Duration::from_secs(cli.startup_timeout.unwrap_or(60)));
    for r in &cli.kernelspec_root {
        b = b.kernelspec_root(r);
    }
    let engine = b.build();
    let verbose = cli.verbose;
    match cli.cmd {
        Cmd::Kernelspec { cmd } => kernelspec(&engine, &cli.kernelspec_root, cmd),
        Cmd::Run(args) => run(&engine, args, verbose).await,
        Cmd::Console { target } => {
            let mut stop = StopSignals::new()?;
            let kernel = Arc::new(open(&engine, &target, verbose).await?);
            let r = tokio::select! {
                r = console::run(kernel.clone()) => r,
                sig = stop.recv() => {
                    stop_kernel(&kernel, &target).await;
                    // The line editor holds a blocking thread on stdin,
                    // which would keep the runtime from shutting down.
                    std::process::exit(128 + sig);
                }
            };
            close(&kernel, &target).await?;
            r.map(|()| ExitCode::SUCCESS)
        }
        Cmd::Execute(args) => execute(&engine, args, verbose).await,
        Cmd::Kernel { kernel, cwd } => standalone_kernel(&engine, &kernel, cwd).await,
        Cmd::Attach {
            connection_file,
            code,
            timeout,
        } => {
            let args = RunArgs {
                target: Target {
                    kernel: None,
                    existing: Some(connection_file),
                    cwd: None,
                },
                code,
                file: vec![],
                json: false,
                timeout,
                interrupt_after_ms: None,
                keep_going: false,
            };
            run(&engine, args, verbose).await
        }
        Cmd::Nb {
            cmd: NbCmd::Clear { notebook, dest },
        } => {
            let mut nb = ganymede_engine::nbformat::read_file(&notebook)
                .with_context(|| format!("reading {}", notebook.display()))?;
            clear_outputs(&mut nb);
            let dest = dest.resolve(&notebook).unwrap_or(Some(notebook.clone()));
            write_notebook(&nb, dest.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Trust {
            notebooks,
            reset,
            notary_dir,
        } => trust(&notebooks, reset, notary_dir),
        // Blocking HTTP and file I/O, off the async workers.
        Cmd::Lab { cmd: Some(cmd), .. } => {
            tokio::task::spawn_blocking(move || lab::run(cmd)).await?
        }
        Cmd::Lab { cmd: None, start } => {
            let lab_start::StartArgs { no_browser, serve } = *start;
            let cfg = server_config(serve, &cli.kernelspec_root, cli.startup_timeout, cli.quiet)?;
            lab_start::run(cfg, no_browser).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Serve(args) => {
            let cfg = server_config(*args, &cli.kernelspec_root, cli.startup_timeout, cli.quiet)?;
            ganymede_server::serve::run(cfg).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Info { json } => info(&engine, json),
    }
}

// ----- kernels -------------------------------------------------------------

async fn open(engine: &Engine, t: &Target, verbose: bool) -> Result<Kernel> {
    let kernel = if let Some(cf) = &t.existing {
        engine.attach(cf).await.with_context(|| {
            if cf.as_os_str().is_empty() {
                "attaching to the newest kernel".to_string()
            } else {
                format!("attaching to {}", cf.display())
            }
        })?
    } else {
        let name = t.kernel.as_deref().expect("clap requires a kernel");
        if name == "fake" {
            let (host, side) = in_process_pair(64);
            FakeKernel::spawn(side, ReplyOrder::ReplyThenIdle);
            engine.adopt(host).await?
        } else {
            engine
                .start_with(
                    name,
                    StartOptions {
                        cwd: t.cwd.clone(),
                        env: vec![],
                    },
                )
                .await
                .with_context(|| format!("starting kernel {name}"))?
        }
    };
    if verbose {
        let info = kernel.info();
        eprintln!(
            "kernel: {} {} (protocol {}, language {})",
            info.implementation,
            info.implementation_version,
            info.protocol_version,
            info.language_info.name
        );
    }
    Ok(kernel)
}

/// Shut down kernels we launched; leave attached ones running.
async fn close(kernel: &Kernel, t: &Target) -> Result<()> {
    if t.existing.is_none() {
        kernel.shutdown().await?;
    }
    Ok(())
}

/// Interrupt the kernel on Ctrl-C until the returned guard is dropped. A
/// second Ctrl-C within [`ESCALATE_WINDOW`] goes to `stop` as SIGINT, so
/// a kernel that ignores interrupts can still be stopped, as with
/// `jupyter console`.
fn interrupt_on_ctrl_c(kernel: &Arc<Kernel>, stop: &StopSignals) -> AbortOnDrop {
    let k = kernel.clone();
    let escalate = stop.escalate.clone();
    AbortOnDrop(tokio::spawn(async move {
        let mut last: Option<tokio::time::Instant> = None;
        while tokio::signal::ctrl_c().await.is_ok() {
            let now = tokio::time::Instant::now();
            if last.is_some_and(|t| now - t < ESCALATE_WINDOW) {
                let _ = escalate.send(libc_sig::INT).await;
                return;
            }
            last = Some(now);
            eprintln!("interrupting (Ctrl-C again to stop)");
            if let Err(e) = k.interrupt().await {
                eprintln!("interrupt failed: {e}");
            }
        }
    }))
}

const ESCALATE_WINDOW: Duration = Duration::from_secs(2);

/// SIGTERM and SIGHUP, for commands that hold a kernel. The kernel runs
/// in its own process group, so a terminal hangup or `timeout(1)` reaches
/// only us; without a handler it would outlive us. Registered before the
/// kernel starts so no signal falls between handlers.
struct StopSignals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hup: tokio::signal::unix::Signal,
    /// A repeated Ctrl-C, from [`interrupt_on_ctrl_c`].
    escalate: tokio::sync::mpsc::Sender<i32>,
    escalated: tokio::sync::mpsc::Receiver<i32>,
}

impl StopSignals {
    fn new() -> Result<Self> {
        let (escalate, escalated) = tokio::sync::mpsc::channel(1);
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(StopSignals {
                term: signal(SignalKind::terminate()).context("SIGTERM handler")?,
                hup: signal(SignalKind::hangup()).context("SIGHUP handler")?,
                escalate,
                escalated,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(StopSignals {
                escalate,
                escalated,
            })
        }
    }

    /// Resolves with the signal number.
    async fn recv(&mut self) -> i32 {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.term.recv() => libc_sig::TERM,
                _ = self.hup.recv() => libc_sig::HUP,
                Some(sig) = self.escalated.recv() => sig,
            }
        }
        #[cfg(not(unix))]
        {
            match self.escalated.recv().await {
                Some(sig) => sig,
                None => std::future::pending().await,
            }
        }
    }
}

mod libc_sig {
    pub const INT: i32 = 2;
    #[cfg(unix)]
    pub const TERM: i32 = 15;
    #[cfg(unix)]
    pub const HUP: i32 = 1;
}

/// On a stop signal: shut down a kernel we launched; interrupt an attached
/// one so it is not left busy with our cell.
async fn stop_kernel(kernel: &Kernel, t: &Target) {
    eprintln!("ganymede: stopping kernel");
    let r = if t.existing.is_some() {
        kernel.interrupt().await
    } else {
        kernel.shutdown().await
    };
    if let Err(e) = r {
        eprintln!("ganymede: {e}");
    }
}

fn exit_on_signal(sig: i32) -> ExitCode {
    ExitCode::from((128 + sig) as u8)
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run(engine: &Engine, args: RunArgs, verbose: bool) -> Result<ExitCode> {
    let mut cells = args.code.clone();
    for f in &args.file {
        cells.push(std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?);
    }
    let stdin_is_code = cells.is_empty();
    if stdin_is_code {
        let mut code = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut code)?;
        cells.push(code);
    }
    let mut stop = StopSignals::new()?;
    let kernel = Arc::new(open(engine, &args.target, verbose).await?);
    let _guard = interrupt_on_ctrl_c(&kernel, &stop);
    let opts = ExecuteOptions {
        // Input comes from the terminal only when stdin is not the code.
        allow_stdin: !stdin_is_code && !args.json,
        stop_on_error: !args.keep_going,
        timeout: (args.timeout > 0).then(|| Duration::from_secs(args.timeout)),
        ..Default::default()
    };
    let r = tokio::select! {
        r = run_cells(&kernel, &args, &cells, &opts, verbose) => r,
        sig = stop.recv() => {
            stop_kernel(&kernel, &args.target).await;
            return Ok(exit_on_signal(sig));
        }
    };
    close(&kernel, &args.target).await?;
    Ok(if r? {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Run each cell; `Ok(true)` when a cell failed.
async fn run_cells(
    kernel: &Arc<Kernel>,
    args: &RunArgs,
    cells: &[String],
    opts: &ExecuteOptions,
    verbose: bool,
) -> Result<bool> {
    let mut failed = false;
    for (i, code) in cells.iter().enumerate() {
        let interrupter = args.interrupt_after_ms.map(|ms| {
            let k = kernel.clone();
            AbortOnDrop(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = k.interrupt().await;
            }))
        });
        let r = if args.json {
            kernel
                .execute_with(code, opts, &mut ganymede_engine::NoHandler)
                .await
        } else {
            let mut term = Terminal {
                stdin_allowed: opts.allow_stdin,
            };
            kernel.execute_with(code, opts, &mut term).await
        };
        drop(interrupter);
        let r = r.with_context(|| format!("cell {i}"))?;
        if args.json {
            println!("{}", render::json_line(i, &r));
        }
        if verbose {
            eprintln!(
                "[{}] {}",
                r.execution_count.map(|n| n.to_string()).unwrap_or_default(),
                r.reply["status"].as_str().unwrap_or("?")
            );
        }
        if !r.is_ok() {
            failed = true;
            if !args.keep_going {
                break;
            }
        }
    }
    Ok(failed)
}

async fn execute(engine: &Engine, args: ExecuteArgs, verbose: bool) -> Result<ExitCode> {
    if args.notebooks.len() > 1 {
        if let Some(o) = &args.dest.output {
            if !o.contains("{notebook_name}") {
                bail!("with several notebooks, --output must contain {{notebook_name}}");
            }
        }
    }
    let opts = ganymede_engine::NotebookOptions {
        timeout: (args.timeout > 0).then(|| Duration::from_secs(args.timeout)),
        allow_errors: args.allow_errors,
        allow_error_names: args.allow_error_name.clone(),
        force_raise_errors: args.force_raise_errors,
        skip_cells_with_tag: args.skip_tag.clone(),
        coalesce_streams: args.coalesce_streams,
        record_timing: !args.no_timing,
        store_widget_state: !args.no_widget_state,
    };
    let mut stop = StopSignals::new()?;
    let mut status = ExitCode::SUCCESS;
    for path in &args.notebooks {
        let mut nb = ganymede_engine::nbformat::read_file(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let name = match &args.kernel {
            Some(k) => k.clone(),
            None => ganymede_engine::notebook_kernel_name(&nb).with_context(|| {
                format!(
                    "{} has no metadata.kernelspec.name; pass --kernel",
                    path.display()
                )
            })?,
        };
        let cwd = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let target = Target {
            kernel: Some(name),
            existing: None,
            cwd,
        };
        eprintln!("executing {}", path.display());
        let kernel = Arc::new(open(engine, &target, verbose).await?);
        let _guard = interrupt_on_ctrl_c(&kernel, &stop);
        let mut on_event = |e: ganymede_engine::exec::Event<'_>| {
            if let (true, ganymede_engine::exec::Event::CellFinished { index, cell, ok }) =
                (verbose, e)
            {
                eprintln!(
                    "  cell {index} [{}] {}",
                    cell.execution_count.unwrap_or_default(),
                    if ok { "ok" } else { "error" }
                );
            }
        };
        let result = tokio::select! {
            r = kernel.run_notebook_with(&mut nb, &opts, &mut on_event) => r,
            sig = stop.recv() => {
                stop_kernel(&kernel, &target).await;
                return Ok(exit_on_signal(sig));
            }
        };
        close(&kernel, &target).await?;
        // Like nbclient, a failed run is not saved: the notebook would
        // look executed when it was not.
        match result {
            Ok(report) => {
                eprintln!(
                    "  {} cells executed, {} errored",
                    report.executed,
                    report.errored.len()
                );
                if let Some(dest) = args.dest.resolve(path) {
                    write_notebook(&nb, dest.as_deref())?;
                }
            }
            Err(e) => {
                eprintln!("  {e}");
                if let ganymede_engine::exec::Error::CellFailed { index, .. } = &e {
                    if let Some(Cell::Code(c)) = nb.cells.get(*index) {
                        for o in &c.outputs {
                            if let ganymede_engine::nbformat::Output::Error(err) = o {
                                for line in &err.traceback {
                                    eprintln!("{}", render::strip_ansi(line));
                                }
                            }
                        }
                    }
                }
                status = ExitCode::FAILURE;
            }
        }
    }
    Ok(status)
}

async fn standalone_kernel(engine: &Engine, name: &str, cwd: Option<PathBuf>) -> Result<ExitCode> {
    let kernel = engine
        .start_with(name, StartOptions { cwd, env: vec![] })
        .await
        .with_context(|| format!("starting kernel {name}"))?;
    let file = kernel
        .connection_file()
        .context("kernel has no connection file")?;
    eprintln!("kernel {name} running; Ctrl-C to stop. Attach with:");
    eprintln!("  ganymede console --existing {}", file.display());
    println!("{}", file.display());
    wait_for_stop().await;
    kernel.shutdown().await?;
    Ok(ExitCode::SUCCESS)
}

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ----- files -----------------------------------------------------------------

fn write_notebook(nb: &Notebook, dest: Option<&Path>) -> Result<()> {
    match dest {
        Some(p) => {
            ganymede_engine::nbformat::write_file(nb, p)
                .with_context(|| format!("writing {}", p.display()))?;
            eprintln!("  wrote {}", p.display());
        }
        None => print!("{}", nb.to_file_string()),
    }
    Ok(())
}

/// `jupyter trust`: the same messages, the same exit status.
fn trust(notebooks: &[PathBuf], reset: bool, dir: Option<PathBuf>) -> Result<ExitCode> {
    use ganymede_contents::trust::{self, Notary, DB_FILE, SECRET_FILE};
    let dir = match dir {
        Some(d) => d,
        None => ks::jupyter_user_data_dir().context("no Jupyter data directory")?,
    };
    if reset {
        let db = dir.join(DB_FILE);
        if db.exists() {
            println!("Removing trusted signature cache: {}", db.display());
            std::fs::remove_file(&db).with_context(|| format!("removing {}", db.display()))?;
        }
        let secret = dir.join(SECRET_FILE);
        println!("Generating new notebook key: {}", secret.display());
        trust::write_secret(&secret, &trust::generate_secret()?)
            .with_context(|| format!("writing {}", secret.display()))?;
        return Ok(ExitCode::SUCCESS);
    }
    let notary = Notary::open(&dir).with_context(|| format!("notary in {}", dir.display()))?;
    let sign = |nb: &Notebook, label: &str| -> Result<()> {
        let nb = serde_json::to_value(nb)?;
        if notary.check_signature(&nb) {
            println!("Notebook already signed: {label}");
        } else {
            println!("Signing notebook: {label}");
            notary.sign(&nb)?;
        }
        Ok(())
    };
    if notebooks.is_empty() {
        let nb = Notebook::from_reader(std::io::stdin().lock()).context("reading stdin")?;
        sign(&nb, "<stdin>")?;
    }
    for path in notebooks {
        if !path.exists() {
            eprintln!("Notebook missing: {}", path.display());
            return Ok(ExitCode::FAILURE);
        }
        let nb = ganymede_engine::nbformat::read_file(path)
            .with_context(|| format!("reading {}", path.display()))?;
        sign(&nb, &path.display().to_string())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What `jupyter nbconvert --clear-output` removes.
fn clear_outputs(nb: &mut Notebook) {
    for cell in &mut nb.cells {
        if let Cell::Code(c) = cell {
            c.outputs.clear();
            c.execution_count = None;
            for key in ["collapsed", "scrolled", "execution"] {
                c.metadata.remove(key);
            }
        }
    }
}

// ----- kernelspecs and info ------------------------------------------------

fn kernelspec(engine: &Engine, extra_roots: &[PathBuf], cmd: KernelspecCmd) -> Result<ExitCode> {
    match cmd {
        KernelspecCmd::List { json } => {
            let specs = engine.kernelspecs()?;
            if json {
                let mut map = serde_json::Map::new();
                for s in specs {
                    map.insert(
                        s.name.clone(),
                        serde_json::json!({
                            "resource_dir": s.resource_dir,
                            "spec": serde_json::to_value(&s)?,
                        }),
                    );
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "kernelspecs": map }))?
                );
            } else if specs.is_empty() {
                eprintln!("no kernelspecs found; `ganymede kernelspec paths` lists where they are looked for");
            } else {
                let w = specs.iter().map(|s| s.name.len()).max().unwrap_or(0);
                let wd = specs
                    .iter()
                    .map(|s| s.display_name.len())
                    .max()
                    .unwrap_or(0);
                for s in specs {
                    println!(
                        "{:<w$}  {:<wd$}  {:<12}  {}",
                        s.name,
                        s.display_name,
                        s.language,
                        s.resource_dir.display()
                    );
                }
            }
        }
        KernelspecCmd::Paths => {
            for r in extra_roots.iter().cloned().chain(ks::jupyter_data_dirs()) {
                println!("{}", r.join("kernels").display());
            }
        }
        KernelspecCmd::Install {
            source,
            name,
            prefix,
            replace,
        } => {
            let data_dir = match prefix {
                Some(p) => p.join("share").join("jupyter"),
                None => ks::jupyter_user_data_dir().context("no user data directory")?,
            };
            let dest = ks::install(&source, name.as_deref(), &data_dir, replace)?;
            println!("{}", dest.display());
        }
        KernelspecCmd::Remove { names } => {
            for n in names {
                let dir = ks::remove(engine.kernelspec_source().as_ref(), &n)?;
                eprintln!("removed {n} from {}", dir.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn info(engine: &Engine, json: bool) -> Result<ExitCode> {
    let specs: Vec<String> = engine.kernelspecs()?.into_iter().map(|s| s.name).collect();
    let v = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": ganymede_engine::protocol::PROTOCOL_VERSION,
        "data_dirs": ks::jupyter_data_dirs(),
        "runtime_dir": ks::jupyter_runtime_dir(),
        "config_dir": ks::jupyter_config_dir(),
        "kernelspecs": specs,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("ganymede {}", v["version"].as_str().unwrap_or_default());
        println!(
            "protocol    {}",
            v["protocol_version"].as_str().unwrap_or_default()
        );
        println!("runtime dir {}", ks::jupyter_runtime_dir().display());
        println!("config dir  {}", ks::jupyter_config_dir().display());
        println!("data dirs:");
        for d in ks::jupyter_data_dirs() {
            println!("  {}", d.display());
        }
        println!("kernelspecs: {}", specs.join(", "));
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("ganymede").chain(args.iter().copied())).unwrap()
    }

    fn serve_config(cli: Cli) -> ganymede_server::config::Config {
        let args = match cli.cmd {
            Cmd::Serve(a) => *a,
            Cmd::Lab {
                cmd: None, start, ..
            } => start.serve,
            _ => panic!("not a server command"),
        };
        server_config(args, &cli.kernelspec_root, cli.startup_timeout, cli.quiet).unwrap()
    }

    #[test]
    fn global_flags_reach_the_server_config() {
        let cfg = serve_config(parse(&[
            "serve",
            "--startup-timeout",
            "5",
            "-q",
            "--kernelspec-root",
            "/k",
            "--port",
            "0",
        ]));
        assert_eq!(cfg.startup_timeout_secs, 5);
        assert!(cfg.quiet);
        assert_eq!(cfg.kernelspec_roots, [PathBuf::from("/k")]);
        assert_eq!(cfg.port, 0);

        let cfg = serve_config(parse(&[
            "-q",
            "--startup-timeout",
            "7",
            "lab",
            "--no-browser",
        ]));
        assert_eq!((cfg.startup_timeout_secs, cfg.quiet), (7, true));
    }

    #[test]
    fn unset_globals_keep_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.toml");
        std::fs::write(&path, "startup_timeout_secs = 9\nquiet = true\n").unwrap();
        let cfg = serve_config(parse(&["serve", "--config", path.to_str().unwrap()]));
        assert_eq!((cfg.startup_timeout_secs, cfg.quiet), (9, true));
        let cfg = serve_config(parse(&["serve"]));
        assert_eq!((cfg.startup_timeout_secs, cfg.quiet), (60, false));
    }

    #[test]
    fn servers_log_at_info_by_default() {
        assert!(parse(&["serve"]).log_filter().starts_with("info"));
        assert!(parse(&["lab"]).log_filter().starts_with("info"));
        assert!(parse(&["serve", "-v"]).log_filter().starts_with("debug"));
        assert!(parse(&["lab", "list"]).log_filter().starts_with("warn"));
        assert!(parse(&["kernelspec", "list"])
            .log_filter()
            .starts_with("warn"));
    }
}
