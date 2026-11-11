//! `ganymede`: the command line for the engine. Every subcommand is a thin
//! layer over `ganymede-engine` (or `ganymede-server` for `serve`).

mod console;
mod crash;
mod grant;
mod help;
mod info;
mod lab;
mod lab_start;
mod mcp;
mod render;
mod ui;
mod update;

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
    version = env!("GANYMEDE_VERSION"),
    about = "Language-agnostic Jupyter engine: kernels, notebooks, and a server",
    styles = help::STYLES
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Search this kernelspec root (a dir holding kernels/) first
    #[arg(long, global = true, value_name = "DIR")]
    kernelspec_root: Vec<PathBuf>,
    /// Discard kernel processes' own stdout and stderr, not cell output
    #[arg(long, short, global = true)]
    quiet: bool,
    /// Report kernel and cell status on stderr; servers log at debug
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Seconds for a new kernel to answer; default 60 or config file
    #[arg(long, global = true, value_name = "SECS", alias = "startup_timeout")]
    startup_timeout: Option<u64>,
}

impl Cli {
    /// Whether this runs a server, which logs at info level by default.
    fn serves(&self) -> bool {
        matches!(
            self.cmd,
            Cmd::Serve(_) | Cmd::Lab { cmd: None, .. } | Cmd::Ui { cmd: None, .. }
        )
    }

    /// The server flags, for `serve` and `lab`: `--log-level` and
    /// `--log-file` apply before the server starts.
    fn serve_args(&self) -> Option<&ganymede_server::serve::ServeArgs> {
        match &self.cmd {
            Cmd::Serve(a) => Some(a),
            Cmd::Lab {
                cmd: None, start, ..
            }
            | Cmd::Ui {
                cmd: None, start, ..
            } => Some(&start.serve),
            _ => None,
        }
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
    /// Run code on a kernel and print its output.
    ///
    /// Each CODE argument and each --file is one cell, run in order. With
    /// neither, stdin is read as one cell. Exits 1 if a cell fails.
    Run(RunArgs),
    /// Interactive console on a kernel.
    ///
    /// Reads stdin as a script when it is not a terminal.
    Console {
        #[command(flatten)]
        target: Target,
    },
    /// Execute notebooks, as jupyter execute does.
    ///
    /// Outputs, execution counts, timings, and `language_info` are written
    /// as nbclient writes them. Without --output or --inplace the result
    /// is not saved, which checks that the notebook runs.
    #[command(alias = "exec")]
    Execute(ExecuteArgs),
    /// Start a kernel for other clients, as jupyter kernel does.
    ///
    /// The kernel keeps running until Ctrl-C. Other clients attach to it by
    /// the connection file this prints.
    Kernel {
        /// Kernelspec name, as `ganymede kernelspec list` shows it.
        #[arg(long, short)]
        kernel: String,
        /// Working directory for the kernel.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Run code on a kernel that is already running.
    ///
    /// The same as `run --existing CONNECTION_FILE CODE...`, but waiting at
    /// most 60 seconds per cell by default. The kernel keeps running
    /// afterwards.
    Attach {
        /// The kernel's connection file, such as the path `ganymede kernel`
        /// printed.
        connection_file: PathBuf,
        /// Code to run; one cell per argument. With none, stdin is one cell.
        code: Vec<String>,
        /// Seconds to wait for each cell. 0 waits forever.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Print a shell completion script.
    #[command(after_long_help = "Examples:\n  \
        ganymede completions bash > ~/.local/share/bash-completion/completions/ganymede\n  \
        ganymede completions zsh > ~/.zfunc/_ganymede")]
    Completions {
        /// The shell to complete for.
        shell: clap_complete::Shell,
    },
    /// Notebook file utilities that need no kernel.
    Nb {
        #[command(subcommand)]
        cmd: NbCmd,
    },
    /// Grant a notebook more or less of the host API than the defaults.
    ///
    /// What a notebook's cells may ask the server for through
    /// `jupyter.host()`: `read=PATH` (other notebooks it may read),
    /// `run=PATH` (notebooks it may run) and `edit_self` (its own cells). Kept by file id beside the server's
    /// file id database in the Jupyter data dir; a running server sees a
    /// change on the next call.
    Grant(grant::GrantArgs),
    /// Trust notebooks' HTML and JavaScript, as jupyter trust does.
    ///
    /// Records each notebook's signature with the key and database Jupyter
    /// uses (`notebook_secret` and `nbsignatures.db` in the Jupyter data
    /// dir), so the server and Jupyter render its outputs unsanitized. With
    /// no NOTEBOOK, one is read from stdin.
    Trust {
        /// Notebooks to sign. With none, one is read from stdin.
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
    /// Serve JupyterLab here and open it; install and remove its builds.
    ///
    /// With no subcommand, serves the current directory with JupyterLab
    /// and opens it in the browser. The subcommands install, list, and
    /// remove JupyterLab's frontend and its extensions.
    #[command(args_conflicts_with_subcommands = true)]
    Lab {
        #[command(subcommand)]
        cmd: Option<lab::LabCmd>,
        #[command(flatten)]
        start: Box<lab_start::StartArgs>,
    },
    /// Serve ganymede-ui here and open it; install and remove its builds.
    ///
    /// ganymede-ui is Ganymede's own notebook app. With no subcommand,
    /// serves the current directory with it and opens it in the browser.
    /// The subcommands install, list, and remove it.
    #[command(args_conflicts_with_subcommands = true)]
    Ui {
        #[command(subcommand)]
        cmd: Option<ui::UiCmd>,
        #[command(flatten)]
        start: Box<lab_start::StartArgs>,
    },
    /// Serve notebook tools to an agent over MCP on stdio.
    ///
    /// Agents that start MCP servers as subprocesses (Claude Code, Codex,
    /// Cursor) run this to open, edit and run notebooks. It uses a running
    /// `ganymede serve --mcp` whose root holds the current directory, so
    /// the agent's edits show live in JupyterLab or ganymede-ui, else
    /// starts a server of its own.
    Mcp(mcp::McpArgs),
    /// Serve the Jupyter REST and websocket API.
    ///
    /// Clients such as JupyterLab and VS Code connect to it as a Jupyter
    /// server.
    Serve(Box<ganymede_server::serve::ServeArgs>),
    /// Show versions and every directory Ganymede uses.
    ///
    /// Prints build details; Jupyter's runtime, config, and data dirs;
    /// Ganymede's data dir with its JupyterLab installs and extensions;
    /// kernelspec roots; and where notebook trust and console history are
    /// kept.
    Info {
        /// Print one JSON object instead.
        #[arg(long)]
        json: bool,
    },
    /// Replace this binary with the latest release, as install.sh does.
    ///
    /// Downloads this platform's archive over HTTPS, checks its SHA-256,
    /// runs the new binary before it replaces anything, and leaves
    /// cdp-kernel, JupyterLab and ganymede-ui as they are.
    /// `GANYMEDE_DOWNLOAD_BASE` points at a mirror of the release downloads.
    Update(update::UpdateArgs),
    /// Panic, to check crash reporting. Debug builds only.
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    DebugPanic,
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
    /// Install a kernelspec directory (one holding kernel.json).
    Install {
        /// Directory holding `kernel.json` (and any logos or resources).
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
        /// Kernelspec names, as `ganymede kernelspec list` shows them.
        #[arg(required = true)]
        names: Vec<String>,
        /// Do not ask first. Without a terminal on stdin nothing is asked.
        #[arg(long, short = 'f', visible_short_alias = 'y')]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NbCmd {
    /// Remove outputs, execution counts, and execution timings.
    Clear {
        /// The notebook to clear. Overwritten unless --output is given.
        notebook: PathBuf,
        #[command(flatten)]
        dest: Dest,
    },
    /// Write the notebook as one self-contained HTML page to share.
    ///
    /// Prose, charts and tables, with the code folded under each result.
    /// The page makes no network requests and runs no script from the
    /// notebook. Your home directory and the notebook's directory are
    /// replaced by `~` and `.` wherever they appear.
    Publish {
        /// The notebook to publish.
        notebook: PathBuf,
        /// Where to write the page; `-` is stdout. Default: the
        /// notebook's path with `.html`.
        #[arg(long, short)]
        output: Option<String>,
        /// How code shows: `folded` under a toggle, `shown`, or `hidden`.
        #[arg(long, default_value = "folded")]
        code: ganymede_engine::nbformat::CodeMode,
        /// A line under the title, such as the date.
        #[arg(long)]
        byline: Option<String>,
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
    /// Added to a launched kernel's environment (`execute -p`).
    #[arg(skip)]
    env: Vec<(String, String)>,
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
    /// instead of rendering output. A cell that could not finish (a
    /// timeout, a dead kernel) gets `"status": "incomplete"` and an
    /// `error`. The kernel's input requests get an empty reply.
    #[arg(long)]
    json: bool,
    /// Seconds to wait for each cell. 0 waits forever.
    #[arg(long, default_value_t = 0)]
    timeout: u64,
    /// Interrupt each cell after this many milliseconds, to check how a
    /// kernel handles an interrupt.
    #[arg(long, value_name = "MS")]
    interrupt_after_ms: Option<u64>,
    /// Keep running cells after one fails.
    #[arg(long)]
    keep_going: bool,
}

#[derive(Args)]
struct ExecuteArgs {
    /// Notebooks to execute, each on its own kernel, in order.
    #[arg(required = true)]
    notebooks: Vec<PathBuf>,
    #[command(flatten)]
    dest: Dest,
    /// Kernelspec name. Defaults to each notebook's
    /// `metadata.kernelspec.name`.
    #[arg(long, short, alias = "kernel_name")]
    kernel: Option<String>,
    /// Record errors and keep going.
    #[arg(long)]
    allow_errors: bool,
    /// Error names that do not stop execution. Repeatable.
    #[arg(long, value_name = "ENAME", alias = "allow-error-names")]
    allow_error_name: Vec<String>,
    /// Stop on every error, overriding the allowances and
    /// `raises-exception` tags.
    #[arg(long)]
    force_raise_errors: bool,
    /// Seconds per cell. 0 waits forever.
    #[arg(long, default_value_t = 0)]
    timeout: u64,
    /// Cells with this tag are skipped.
    #[arg(
        long,
        default_value = "skip-execution",
        aliases = ["skip-cells-with-tag", "skip_cells_with_tag"]
    )]
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
    /// A parameter for the run, `NAME=VALUE`. Repeatable. VALUE is read
    /// as JSON when it parses (`7`, `true`, `[1,2]`), else as a string.
    /// The kernel gets them as a JSON object in `GANYMEDE_PARAMS`.
    #[arg(long = "param", short = 'p', value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Parameters from a file holding a JSON object; `-p` wins.
    #[arg(long, value_name = "FILE")]
    params_file: Option<PathBuf>,
    /// Print each notebook's named results on stdout, as one JSON line:
    /// `{"path", "status", "results"}`.
    #[arg(long)]
    results: bool,
    /// A host API grant for the notebooks' kernels (docs/host-api.md):
    /// `read=PATH` a path they may read, `run=PATH` a notebook they may
    /// run. Repeatable. A key given replaces the config file's `[host]`
    /// default; PATH is relative to the current directory, `""` is all
    /// of it.
    #[arg(long = "grant", value_name = "KEY=PATH")]
    grants: Vec<String>,
}

/// The host API grants for `ganymede execute`: `base`, the config file's
/// `[host]`, with each key `grants` names replaced. Its kernels have no
/// `self.*` calls: the run writes the notebook itself when it ends.
fn execute_grants(
    base: ganymede_server::host::HostConfig,
    grants: &[String],
) -> Result<ganymede_server::host::HostConfig> {
    let mut read: Option<Vec<String>> = None;
    let mut run: Option<Vec<String>> = None;
    for g in grants {
        let Some((key, path)) = g.split_once('=') else {
            return Err(Usage(format!("--grant {g:?}: use read=PATH or run=PATH")).into());
        };
        let list = match key {
            "read" => &mut read,
            "run" => &mut run,
            _ => {
                return Err(Usage(format!(
                    "--grant {g:?}: use read=PATH or run=PATH (execute has no edit_self)"
                ))
                .into())
            }
        };
        let path = grant::grant_path(key, path).map_err(|e| Usage(format!("--grant: {e}")))?;
        let list = list.get_or_insert_with(Vec::new);
        if !list.contains(&path) {
            list.push(path);
        }
    }
    Ok(ganymede_server::host::HostConfig {
        read: read.unwrap_or(base.read),
        run: run.unwrap_or(base.run),
        edit_self: false,
    })
}

/// What `ganymede execute` answers its kernels' host calls with, rooted
/// at the current directory. `None`, with a warning, when it can't start:
/// the notebooks still run, with no host comm.
fn execute_host(
    grants: &[String],
    kernelspec_roots: &[PathBuf],
    startup_timeout: Option<u64>,
    quiet: bool,
) -> Result<Option<ganymede_server::serve::LocalHost>> {
    // A config file that doesn't parse leaves the run without a host comm,
    // as a host that can't start does: `execute` worked without one before.
    let mut cfg = match crash::config_text(None)
        .map(|text| ganymede_server::config::Config::from_toml(&text))
    {
        None => Default::default(),
        Some(Ok(cfg)) => cfg,
        Some(Err(e)) => {
            execute_grants(Default::default(), grants)?;
            eprintln!("ganymede: no host API for this run: the config file: {e}");
            return Ok(None);
        }
    };
    cfg.host = execute_grants(cfg.host, grants)?;
    // As `serve` takes them: the config's roots, then the flags'.
    for r in kernelspec_roots {
        if !cfg.kernelspec_roots.contains(r) {
            cfg.kernelspec_roots.push(r.clone());
        }
    }
    if let Some(t) = startup_timeout {
        cfg.startup_timeout_secs = t;
    }
    cfg.quiet |= quiet;
    let root = std::env::current_dir().context("the current directory")?;
    match ganymede_server::serve::LocalHost::new(&cfg, &root) {
        Ok(h) => Ok(Some(h)),
        Err(e) => {
            eprintln!("ganymede: no host API for this run: {}", error_chain(&e));
            Ok(None)
        }
    }
}

/// `path`'s API path under the current directory, as `ganymede grant`
/// finds it, if it is under it.
fn api_path_of(path: &Path) -> Option<String> {
    let root = std::env::current_dir().ok()?.canonicalize().ok()?;
    grant::api_path(&root, path).ok()
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
    let cli = {
        use clap::FromArgMatches;
        Cli::from_arg_matches(&help::command().get_matches()).unwrap_or_else(|e| e.exit())
    };
    let config = crash::config_text(cli.serve_args().and_then(|a| a.config.as_deref()));
    let opted_in = crash::opted_in(std::env::var_os(crash::ENV).as_deref(), config.as_deref());
    if opted_in {
        // Before any thread starts: kernels the server launches inherit
        // this, and cdp-kernel reports its own crashes only when it is set.
        std::env::set_var(crash::KERNEL_ENV, "1");
    }
    #[cfg(feature = "crash-reporting")]
    let (_reporting, extra) = crash::maybe_start(opted_in, crash::DSN).unzip();
    #[cfg(not(feature = "crash-reporting"))]
    let extra = None;
    if let Err(e) =
        ganymede_server::serve::init_logging_with(cli.log_filter(), cli.serve_args(), extra)
    {
        eprintln!("ganymede: {}", error_chain(&e));
        return ExitCode::FAILURE;
    }
    if opted_in && !cfg!(feature = "crash-reporting") {
        tracing::warn!(
            "crash reporting is on, but this build can't send reports (built without the crash-reporting feature)"
        );
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ganymede: {}", error_chain(&e));
            if e.is::<Usage>() {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// A usage error found after parsing, which exits 2 as clap's own do.
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

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
        Cmd::Execute(args) => {
            let host = execute_host(
                &args.grants,
                &cli.kernelspec_root,
                cli.startup_timeout,
                cli.quiet,
            )?;
            let r = execute(&engine, args, verbose, host.as_ref()).await;
            if let Some(h) = &host {
                h.shutdown().await;
            }
            r
        }
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
                    env: vec![],
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
        Cmd::Nb {
            cmd:
                NbCmd::Publish {
                    notebook,
                    output,
                    code,
                    byline,
                },
        } => {
            publish(&notebook, output.as_deref(), code, byline)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Completions { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "ganymede",
                &mut std::io::stdout(),
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Trust {
            notebooks,
            reset,
            notary_dir,
        } => trust(&notebooks, reset, notary_dir),
        Cmd::Grant(args) => tokio::task::spawn_blocking(move || grant::run(args)).await?,
        // Blocking HTTP and file I/O, off the async workers.
        Cmd::Lab { cmd: Some(cmd), .. } => {
            tokio::task::spawn_blocking(move || lab::run(cmd)).await?
        }
        Cmd::Lab { cmd: None, start } => {
            let lab_start::StartArgs {
                no_browser,
                no_redirect_file,
                serve,
            } = *start;
            let browser = lab_start::Browser {
                no_browser,
                no_redirect_file,
            };
            let cfg = server_config(serve, &cli.kernelspec_root, cli.startup_timeout, cli.quiet)?;
            lab_start::run(cfg, browser, true).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Ui { cmd: Some(cmd), .. } => tokio::task::spawn_blocking(move || ui::run(cmd)).await?,
        Cmd::Ui { cmd: None, start } => {
            let lab_start::StartArgs {
                no_browser,
                no_redirect_file,
                serve,
            } = *start;
            let browser = lab_start::Browser {
                no_browser,
                no_redirect_file,
            };
            let mut cfg =
                server_config(serve, &cli.kernelspec_root, cli.startup_timeout, cli.quiet)?;
            if cfg.static_dir.is_none() {
                cfg.static_dir = Some(ui::newest()?);
            }
            lab_start::run(cfg, browser, false).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Serve(args) => {
            let cfg = server_config(*args, &cli.kernelspec_root, cli.startup_timeout, cli.quiet)?;
            ganymede_server::serve::run(cfg).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Mcp(args) => {
            mcp::run(args, &cli.kernelspec_root, cli.startup_timeout).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Info { json } => info::run(&engine, &cli.kernelspec_root, json),
        Cmd::Update(args) => tokio::task::spawn_blocking(move || update::run(args)).await?,
        #[cfg(debug_assertions)]
        Cmd::DebugPanic => panic!("ganymede debug-panic: a test crash"),
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
                        env: t.env.clone(),
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
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut code).context("reading stdin")?;
        if code.trim().is_empty() {
            return Err(Usage("no code: pass CODE or --file, or pipe code on stdin".into()).into());
        }
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
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                let e = anyhow::Error::from(e).context(format!("cell {i}"));
                if args.json {
                    // So a consumer can tell "stopped here" from "ended".
                    println!(
                        "{}",
                        serde_json::json!({
                            "cell": i,
                            "status": "incomplete",
                            "error": error_chain(&e),
                        })
                    );
                }
                return Err(e);
            }
        };
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

/// Refuse, before running anything, notebooks that `--output` would save
/// to one file, such as `a/x.ipynb` and `b/x.ipynb` with
/// `{notebook_name}-out.ipynb`: each would overwrite the one before.
fn same_output(dest: &Dest, notebooks: &[PathBuf]) -> Result<()> {
    if dest.inplace {
        return Ok(());
    }
    let mut seen: std::collections::HashMap<PathBuf, &Path> = Default::default();
    for input in notebooks {
        let Some(Some(out)) = dest.resolve(input) else {
            continue;
        };
        let key = std::path::absolute(&out).unwrap_or(out.clone());
        if let Some(first) = seen.insert(key, input) {
            bail!(
                "{} and {} would both be saved to {}",
                first.display(),
                input.display(),
                out.display()
            );
        }
    }
    Ok(())
}

async fn execute(
    engine: &Engine,
    args: ExecuteArgs,
    verbose: bool,
    host: Option<&ganymede_server::serve::LocalHost>,
) -> Result<ExitCode> {
    if args.notebooks.len() > 1 {
        if let Some(o) = &args.dest.output {
            if !o.contains("{notebook_name}") {
                bail!("with several notebooks, --output must contain {{notebook_name}}");
            }
        }
    }
    same_output(&args.dest, &args.notebooks)?;
    if args.results && args.dest.output.as_deref() == Some("-") {
        return Err(Usage("--results and --output - both write to stdout".into()).into());
    }
    let params = run_params(&args.params, args.params_file.as_deref())?;
    // Always set, `{}` without parameters, so a kernel never inherits a
    // parent run's GANYMEDE_PARAMS (a notebook that runs another).
    let env = vec![(
        ganymede_engine::nbformat::PARAMS_ENV.to_string(),
        params_env(params.as_ref().unwrap_or(&Default::default()))?,
    )];
    let opts = ganymede_engine::NotebookOptions {
        timeout: (args.timeout > 0).then(|| Duration::from_secs(args.timeout)),
        allow_errors: args.allow_errors,
        allow_error_names: args.allow_error_name.clone(),
        force_raise_errors: args.force_raise_errors,
        skip_cells_with_tag: args.skip_tag.clone(),
        coalesce_streams: args.coalesce_streams,
        record_timing: !args.no_timing,
        store_widget_state: !args.no_widget_state,
        // As `jupyter execute`: a cell that asks for input fails.
        allow_stdin: false,
        store_history: true,
    };
    let mut stop = StopSignals::new()?;
    let mut status = ExitCode::SUCCESS;
    // Like a failing notebook, one that can't be read or started is
    // reported and the rest still run; the exit status says so.
    for path in &args.notebooks {
        let mut nb = match ganymede_engine::nbformat::read_file(path)
            .with_context(|| format!("reading {}", path.display()))
        {
            Ok(nb) => nb,
            Err(e) => {
                eprintln!("ganymede: {}", error_chain(&e));
                print_results(args.results, path, None, Some(&e));
                status = ExitCode::FAILURE;
                continue;
            }
        };
        ganymede_engine::nbformat::record_params(&mut nb, params.as_ref());
        let name = match &args.kernel {
            Some(k) => k.clone(),
            None => match ganymede_engine::notebook_kernel_name(&nb) {
                Some(n) => n,
                None => {
                    let e = anyhow::anyhow!(
                        "{} has no metadata.kernelspec.name; pass --kernel",
                        path.display()
                    );
                    eprintln!("ganymede: {e}");
                    print_results(args.results, path, None, Some(&e));
                    status = ExitCode::FAILURE;
                    continue;
                }
            },
        };
        let cwd = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf);
        let target = Target {
            kernel: Some(name),
            existing: None,
            cwd,
            env: env.clone(),
        };
        eprintln!("executing {}", path.display());
        let kernel = match open(engine, &target, verbose).await {
            Ok(k) => Arc::new(k),
            Err(e) => {
                eprintln!("  {}", error_chain(&e));
                print_results(args.results, path, None, Some(&e));
                status = ExitCode::FAILURE;
                continue;
            }
        };
        let _guard = interrupt_on_ctrl_c(&kernel, &stop);
        let _host = host.map(|h| h.attach(&kernel.session(), api_path_of(path)));
        // Cells this run started: only their outputs are this run's
        // results. A cell after a failure, or one skipped by tag, still
        // holds what an earlier run saved.
        let mut ran = std::collections::BTreeSet::new();
        let mut on_event = |e: ganymede_engine::exec::Event<'_>| {
            if let ganymede_engine::exec::Event::CellStarted { index, .. } = e {
                ran.insert(index);
            }
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
                print_results(args.results, path, None, Some(&anyhow::anyhow!("stopped by a signal")));
                return Ok(exit_on_signal(sig));
            }
        };
        if let Err(e) = close(&kernel, &target).await {
            print_results(args.results, path, None, Some(&e));
            return Err(e);
        }
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
                    if let Err(e) = write_notebook(&nb, dest.as_deref()) {
                        print_results(args.results, path, Some((&nb, &ran)), Some(&e));
                        return Err(e);
                    }
                }
                print_results(args.results, path, Some((&nb, &ran)), None);
            }
            Err(e) => {
                eprintln!("  {e}");
                if let ganymede_engine::exec::Error::CellFailed { index, .. } = &e {
                    if let Some(Cell::Code(c)) = nb.cells.get(*index) {
                        for o in &c.outputs {
                            if let ganymede_engine::nbformat::Output::Error(err) = o {
                                for line in &err.traceback {
                                    eprintln!("{}", ganymede_engine::nbformat::strip_ansi(line));
                                }
                            }
                        }
                    }
                }
                // The results published before the failure still count.
                print_results(
                    args.results,
                    path,
                    Some((&nb, &ran)),
                    Some(&anyhow::Error::from(e)),
                );
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

/// `--params-file`, then each `-p NAME=VALUE` over it. `None` when neither
/// was given: `execute` then passes `{}` and removes an earlier run's
/// `metadata.ganymede.params`.
fn run_params(
    pairs: &[String],
    file: Option<&Path>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    if pairs.is_empty() && file.is_none() {
        return Ok(None);
    }
    let mut params = match file {
        Some(f) => {
            let text =
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            match serde_json::from_str(&text) {
                Ok(serde_json::Value::Object(m)) => m,
                Ok(_) => bail!("{} must hold a JSON object", f.display()),
                Err(e) => bail!("{} is not JSON: {e}", f.display()),
            }
        }
        None => serde_json::Map::new(),
    };
    for pair in pairs {
        let Some((name, value)) = pair.split_once('=').filter(|(n, _)| !n.is_empty()) else {
            return Err(Usage(format!("-p {pair}: expected NAME=VALUE")).into());
        };
        let value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        params.insert(name.to_string(), value);
    }
    Ok(Some(params))
}

fn params_env(params: &serde_json::Map<String, serde_json::Value>) -> Result<String> {
    ganymede_engine::nbformat::params_env(params).map_err(anyhow::Error::msg)
}

/// `--results`: one JSON line per notebook, with the named results that
/// the cells this run started published (#650), also when a later cell
/// failed.
fn print_results(
    on: bool,
    path: &Path,
    run: Option<(&Notebook, &std::collections::BTreeSet<usize>)>,
    error: Option<&anyhow::Error>,
) {
    if !on {
        return;
    }
    let results = run
        .map(|(nb, ran)| {
            ganymede_engine::nbformat::named_results_of(ran.iter().flat_map(|&i| {
                match nb.cells.get(i) {
                    Some(Cell::Code(c)) => c.outputs.as_slice(),
                    _ => &[],
                }
            }))
        })
        .unwrap_or_default();
    let mut line = serde_json::json!({
        "path": path.to_string_lossy(),
        "status": if error.is_some() { "error" } else { "ok" },
        "results": results,
    });
    if let Some(e) = error {
        line["error"] = error_chain(e).into();
    }
    println!("{line}");
}

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

/// `ganymede nb publish`: the notebook as an HTML page.
fn publish(
    notebook: &Path,
    output: Option<&str>,
    code: ganymede_engine::nbformat::CodeMode,
    byline: Option<String>,
) -> Result<()> {
    use ganymede_engine::nbformat::{to_html, HtmlOptions};
    let nb = ganymede_engine::nbformat::read_file(notebook)
        .with_context(|| format!("reading {}", notebook.display()))?;
    let name = notebook
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "notebook".to_owned());
    let dir = std::path::absolute(notebook)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let redact = ganymede_server::routes::publish_redactions(dir.as_deref(), None);
    let export = to_html(
        &nb,
        &name,
        &HtmlOptions {
            code,
            byline,
            redact,
        },
    );
    for skipped in &export.skipped {
        eprintln!("leaving out {skipped}: bad base64");
    }
    if let Some(o) = output.filter(|o| *o != "-") {
        let lower = o.to_ascii_lowercase();
        if !lower.ends_with(".html") && !lower.ends_with(".htm") {
            bail!("the page is HTML: give -o a name ending in .html, not {o}");
        }
    }
    match output {
        Some("-") => {
            use std::io::Write as _;
            std::io::stdout().write_all(export.body.as_bytes())?;
        }
        _ => {
            let dest = match output {
                Some(o) => PathBuf::from(o),
                None => notebook.with_extension("html"),
            };
            std::fs::write(&dest, export.body)
                .with_context(|| format!("writing {}", dest.display()))?;
            eprintln!("Published {}", dest.display());
        }
    }
    Ok(())
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

// ----- kernelspecs ------------------------------------------------

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
        KernelspecCmd::Remove { names, force } => {
            if !force && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                eprintln!("Kernel specs to remove:");
                for n in &names {
                    eprintln!("  {n}");
                }
                eprint!("Remove {} kernel specs [y/N]: ", names.len());
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !answer.trim().eq_ignore_ascii_case("y") {
                    eprintln!("not removed");
                    return Ok(ExitCode::FAILURE);
                }
            }
            for n in names {
                let dir = ks::remove(engine.kernelspec_source().as_ref(), &n)?;
                eprintln!("removed {n} from {}", dir.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_grants_replace_the_keys_they_name() {
        let base = ganymede_server::host::HostConfig {
            read: vec![String::new()],
            edit_self: true,
            run: vec!["reports".into()],
        };
        let g = execute_grants(base.clone(), &[]).unwrap();
        assert_eq!(
            (g.read, g.run, g.edit_self),
            (vec![String::new()], vec!["reports".into()], false)
        );
        let g = execute_grants(
            base.clone(),
            &[
                "read=docs/".into(),
                "read=./journal".into(),
                "read=docs".into(),
            ],
        )
        .unwrap();
        assert_eq!(g.read, ["docs", "journal"]);
        assert_eq!(g.run, ["reports"]);
        let g = execute_grants(base.clone(), &["run=".into()]).unwrap();
        assert_eq!(g.run, [""]);
        for bad in [
            "edit_self=true",
            "read",
            "write=x",
            "read=../up",
            "run=a/../../b",
        ] {
            assert!(
                execute_grants(base.clone(), &[bad.into()]).is_err(),
                "{bad}"
            );
        }
    }

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

    #[test]
    fn log_level_and_file_reach_serve_and_lab() {
        let cli = parse(&["serve", "--log-level", "WARNING", "--log-file", "/l"]);
        let args = cli.serve_args().unwrap();
        assert_eq!(args.log_level, Some(tracing::Level::WARN));
        assert_eq!(args.log_file, Some(PathBuf::from("/l")));
        let cli = parse(&["lab", "--log-level", "debug"]);
        assert_eq!(
            cli.serve_args().unwrap().log_level,
            Some(tracing::Level::DEBUG)
        );
        assert!(parse(&["lab", "list"]).serve_args().is_none());
    }
}
